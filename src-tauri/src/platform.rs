//! Platform detection and package-manager command building.
//!
//! Every install step is built as a `Command` that the caller executes, so the same
//! code path serves both `--dry-run` (print) and real execution.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// The release binary has no console (`windows_subsystem = "windows"`), so without this flag
/// every child it spawns (winget, cmd.exe for `code.cmd`, ...) opens its own blank console.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// winget exits non-zero when there is nothing left to do. These mean "already there", not
/// failure (see winget-cli's returnCodes.md); the caller re-probes to confirm.
const WINGET_NOTHING_TO_DO: [i32; 3] = [
    0x8A15_002B_u32 as i32, // APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE
    0x8A15_0061_u32 as i32, // APPINSTALLER_CLI_ERROR_PACKAGE_ALREADY_INSTALLED
    0x8A15_010D_u32 as i32, // APPINSTALLER_CLI_ERROR_INSTALL_ALREADY_INSTALLED
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// A single command to run, renderable for dry-run output.
#[derive(Debug, Clone, Serialize)]
pub struct CmdSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Human-readable one-liner shown in the UI log.
    pub display: String,
    /// Non-zero exit codes that still count as success.
    #[serde(skip)]
    ok_exit_codes: Vec<i32>,
}

impl CmdSpec {
    pub fn new(program: &str, args: &[&str]) -> Self {
        let display = if args.is_empty() {
            program.to_string()
        } else {
            format!("{} {}", program, args.join(" "))
        };
        CmdSpec {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            display,
            ok_exit_codes: Vec::new(),
        }
    }

    fn also_ok_on(mut self, codes: &[i32]) -> Self {
        self.ok_exit_codes = codes.to_vec();
        self
    }

    pub fn run(&self) -> Result<String, String> {
        self.run_streaming(&mut |_, _| {})
    }

    /// Run to completion, passing each readable output line to `on_line(line, transient)`
    /// as it arrives. `transient` marks a line the tool redraws in place (a progress bar),
    /// which the UI shows but does not keep.
    pub fn run_streaming(&self, on_line: &mut dyn FnMut(&str, bool)) -> Result<String, String> {
        let mut child = command(&self.program)
            .args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to run `{}`: {e}", self.display))?;

        // One reader per pipe so a chatty stderr can never block stdout (and vice versa).
        let (tx, rx) = mpsc::channel();
        if let Some(pipe) = child.stdout.take() {
            spawn_reader(pipe, Stream::Out, tx.clone());
        }
        if let Some(pipe) = child.stderr.take() {
            spawn_reader(pipe, Stream::Err, tx.clone());
        }
        drop(tx);

        let (mut stdout, mut stderr) = (String::new(), String::new());
        for (stream, raw, redrawn) in rx {
            let Some(line) = clean_line(&raw) else {
                continue;
            };
            let transient = redrawn || line.contains('%');
            if !transient {
                let kept = if stream == Stream::Err {
                    &mut stderr
                } else {
                    &mut stdout
                };
                kept.push_str(&line);
                kept.push('\n');
            }
            on_line(&line, transient);
        }

        let status = child
            .wait()
            .map_err(|e| format!("failed to run `{}`: {e}", self.display))?;
        let (stdout, stderr) = (stdout.trim().to_string(), stderr.trim().to_string());
        if !status.success()
            && !status
                .code()
                .is_some_and(|c| self.ok_exit_codes.contains(&c))
        {
            return Err(if stderr.is_empty() {
                format!("`{}` exited with {}", self.display, status)
            } else {
                format!("`{}` failed: {}", self.display, stderr)
            });
        }
        Ok(if stdout.is_empty() { stderr } else { stdout })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Out,
    Err,
}

fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    stream: Stream,
    tx: mpsc::Sender<(Stream, String, bool)>,
) {
    thread::spawn(move || {
        let mut splitter = LineSplitter::default();
        let mut send = |seg: Vec<u8>, redrawn: bool| {
            let _ = tx.send((stream, String::from_utf8_lossy(&seg).into_owned(), redrawn));
        };
        let mut buf = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            splitter.push(&buf[..n], &mut send);
        }
        splitter.finish(&mut send);
    });
}

/// Splits raw child output on `\n` and `\r`. Installers repaint progress with a bare `\r`,
/// so waiting for `\n` would show nothing until the download ends. A segment closed by a
/// bare `\r` was redrawn in place; `\r\n` is just a Windows line ending.
#[derive(Default)]
struct LineSplitter {
    cur: Vec<u8>,
    /// A segment closed by `\r`, held until the next byte says whether `\n` follows.
    held: Option<Vec<u8>>,
}

impl LineSplitter {
    fn push(&mut self, bytes: &[u8], emit: &mut impl FnMut(Vec<u8>, bool)) {
        for &b in bytes {
            if let Some(seg) = self.held.take() {
                emit(seg, b != b'\n');
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' => emit(std::mem::take(&mut self.cur), false),
                b'\r' => self.held = Some(std::mem::take(&mut self.cur)),
                _ => self.cur.push(b),
            }
        }
    }

    fn finish(&mut self, emit: &mut impl FnMut(Vec<u8>, bool)) {
        if let Some(seg) = self.held.take() {
            emit(seg, true);
        }
        if !self.cur.is_empty() {
            emit(std::mem::take(&mut self.cur), false);
        }
    }
}

/// One output line made readable: ANSI escapes, control characters and the box/block glyphs
/// of progress bars removed. `None` when nothing is left worth showing, which also drops
/// winget's `-\|/` spinner frames and empty bars.
fn clean_line(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\u{2500}'..='\u{259F}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let line = out.trim();
    line.chars()
        .any(char::is_alphanumeric)
        .then(|| line.to_string())
}

/// Consume the rest of an escape sequence whose ESC has already been read.
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars>) {
    match chars.next() {
        // CSI: parameters, then one final byte in `@`..=`~`.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC: runs to BEL or ESC `\`.
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// A tool we may need to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Vscode,
    Git,
    Node,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Vscode => "Visual Studio Code",
            Tool::Git => "Git",
            Tool::Node => "Node.js LTS",
        }
    }
}

/// Result of probing for an installed tool.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub tool: Tool,
    pub found: bool,
    /// Version string if we could read one.
    pub version: Option<String>,
    /// Path to the primary executable, if found.
    pub path: Option<String>,
    /// Set when the tool is present but older than the course kit supports. A present-but-old
    /// Node is worse than a missing one: the wizard would otherwise report "already installed"
    /// and skip it, and the student hits confusing failures mid-class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub too_old: Option<String>,
}

/// The oldest major Node the class kit runs on. Node 18 is EOL and breaks the kit.
pub const MIN_NODE_MAJOR: u64 = 20;

/// Parse the leading integer out of strings like `v20.11.1`, `20.11.1`, `v8.3.0`.
fn major_version(raw: &str) -> Option<u64> {
    raw.trim()
        .trim_start_matches('v')
        .split(['.', '-', ' '])
        .next()
        .and_then(|s| s.parse::<u64>().ok())
}

/// Directories where package managers drop binaries but this process may not look.
///
/// macOS: launchd hands a double-clicked `.app` a bare `/usr/bin:/bin:/usr/sbin:/sbin` and
/// never sources `~/.zprofile`, so without these Homebrew (and everything it installs) is
/// invisible even when it works fine in Terminal.
///
/// Windows: a process keeps the PATH it started with, so after winget installs Git or Node
/// (and updates the registry PATH) this wizard still can't see them. Hence the standard
/// install dirs plus whatever the registry PATH holds right now.
///
/// No-op on Linux.
fn well_known_bin_dirs_for(os: Os) -> Vec<PathBuf> {
    match os {
        Os::Macos => ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"]
            .iter()
            .map(PathBuf::from)
            .collect(),
        Os::Windows => {
            let env = |key: &str| std::env::var(key).ok();
            let mut dirs = windows_install_dirs(
                env("ProgramFiles").as_deref(),
                env("ProgramFiles(x86)").as_deref(),
                env("LOCALAPPDATA").as_deref(),
            );
            dirs.extend(path_entries(&registry_path_values(), env));
            dirs
        }
        Os::Linux => Vec::new(),
    }
}

/// Where winget (and the installers behind it) put Git, Node and VS Code. A root whose
/// environment variable is unset is skipped.
fn windows_install_dirs(
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    local_app_data: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for root in [program_files, program_files_x86].into_iter().flatten() {
        let root = Path::new(root);
        dirs.push(root.join("Git").join("cmd"));
        dirs.push(root.join("nodejs"));
        dirs.push(root.join("Microsoft VS Code").join("bin"));
    }
    if let Some(local) = local_app_data {
        let programs = Path::new(local).join("Programs");
        dirs.push(programs.join("Microsoft VS Code").join("bin"));
        dirs.push(programs.join("Git").join("cmd"));
    }
    dirs
}

/// The machine and user PATH values straight from the registry. Read with `winreg` rather
/// than `reg query` because `command()` computes PATH itself, so spawning `reg` from here
/// would recurse.
#[cfg(target_os = "windows")]
fn registry_path_values() -> Vec<String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    [
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
        (HKEY_CURRENT_USER, "Environment"),
    ]
    .into_iter()
    .filter_map(|(hive, key)| {
        let key = RegKey::predef(hive).open_subkey(key).ok()?;
        key.get_value::<String, _>("Path").ok()
    })
    .collect()
}

#[cfg(not(target_os = "windows"))]
fn registry_path_values() -> Vec<String> {
    Vec::new()
}

/// Directories listed in Windows-style PATH values (`;`-separated, `%VAR%` references).
fn path_entries(values: &[String], env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    values
        .iter()
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(expand_env_vars(entry, &env)))
        .collect()
}

/// Expand `%VAR%` references. Unknown variables are left as written.
fn expand_env_vars(s: &str, env: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after
            .find('%')
            .and_then(|end| env(&after[..end]).map(|value| (end, value)))
        {
            Some((end, value)) => {
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Well-known dirs that actually exist right now. Checked on every call (not cached) so
/// a Homebrew installed mid-run becomes visible without restarting the app.
fn well_known_bin_dirs() -> Vec<PathBuf> {
    well_known_bin_dirs_for(Os::current())
        .into_iter()
        .filter(|d| d.is_dir())
        .collect()
}

/// `existing` with `extra` prepended (so brew's tools win over the system shims).
/// With nothing to add, `existing` comes back untouched rather than re-encoded.
fn path_with_dirs(existing: Option<OsString>, extra: &[PathBuf]) -> OsString {
    let existing = existing.unwrap_or_default();
    if extra.is_empty() {
        return existing;
    }
    let mut dirs: Vec<PathBuf> = extra.to_vec();
    dirs.extend(std::env::split_paths(&existing).filter(|d| !extra.contains(d)));
    // join_paths only fails if a dir contains the separator; keep the original PATH then.
    std::env::join_paths(dirs).unwrap_or(existing)
}

/// The PATH every lookup and child command should use: the process PATH plus the
/// well-known bin dirs. Computed rather than written back with `set_var`, so there is no
/// global mutation to race with Tauri's threads.
pub fn search_path() -> OsString {
    path_with_dirs(std::env::var_os("PATH"), &well_known_bin_dirs())
}

/// The one place child processes are created: corrected PATH, no console window on
/// Windows, and no stdin so anything that wants a password (sudo, installers) fails fast
/// instead of hanging a GUI with no TTY.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    // Only override when the corrected PATH differs; otherwise leave the env alone.
    let path = search_path();
    if std::env::var_os("PATH").unwrap_or_default() != path {
        cmd.env("PATH", path);
    }
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Where `brew` lives under `root`. Apple silicon first, then Intel. Parameterised on
/// `root` only so tests can point it at a temp dir.
fn brew_candidates(root: &Path) -> [PathBuf; 2] {
    [
        under(root, "opt/homebrew/bin/brew"),
        under(root, "usr/local/bin/brew"),
    ]
}

/// Locate Homebrew by absolute path, because PATH is useless for bundled macOS apps
/// (see `well_known_bin_dirs_for`). `which` is the last resort for custom installs.
pub fn find_brew() -> Option<String> {
    find_brew_under(Path::new("/"), which)
}

fn find_brew_under(root: &Path, fallback: impl FnOnce(&str) -> Option<String>) -> Option<String> {
    brew_candidates(root)
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| fallback("brew"))
}

/// Where the official installer puts brew on this CPU. Used to build the install command
/// before brew exists; `install_command` is rebuilt once the installer has run.
fn default_brew_path() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "/opt/homebrew/bin/brew"
    } else {
        "/usr/local/bin/brew"
    }
}

/// Official Homebrew installer, run non-interactively. A GUI has no TTY, so NONINTERACTIVE
/// stops it prompting; it then needs passwordless sudo and aborts (rather than hangs, as
/// stdin is null) when it doesn't have it. `None` when not on macOS or brew is present.
pub fn brew_bootstrap_command() -> Option<CmdSpec> {
    if Os::current() != Os::Macos || find_brew().is_some() {
        return None;
    }
    Some(brew_installer_spec())
}

/// The installer command itself, split out so its shape can be checked on any machine.
fn brew_installer_spec() -> CmdSpec {
    CmdSpec::new(
        "/bin/bash",
        &[
            "-c",
            "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL --connect-timeout 15 \
             https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"",
        ],
    )
}

/// Make sure Homebrew exists, installing it if needed. The `Err` is a short,
/// student-facing reason, not raw installer output.
pub fn ensure_brew(on_line: &mut dyn FnMut(&str, bool)) -> Result<(), String> {
    let Some(bootstrap) = brew_bootstrap_command() else {
        return Ok(());
    };
    bootstrap
        .run_streaming(on_line)
        .map_err(|e| brew_install_error(&e))?;
    // Don't trust the exit code alone: later steps need the binary itself.
    if find_brew().is_none() {
        return Err("The Homebrew installer finished but brew still isn't there.".into());
    }
    Ok(())
}

/// Turn raw installer stderr into one sentence a student can act on.
fn brew_install_error(raw: &str) -> String {
    let raw = raw.to_lowercase();
    if ["sudo", "administrator", "password", "terminal is required"]
        .iter()
        .any(|k| raw.contains(k))
    {
        "Homebrew needs an administrator password, which this app can't type for you.".into()
    } else if ["could not resolve", "curl:", "connection", "timed out"]
        .iter()
        .any(|k| raw.contains(k))
    {
        "Couldn't download Homebrew. Check your internet connection.".into()
    } else {
        "Homebrew couldn't be installed automatically.".into()
    }
}

/// Locate the `code` CLI. macOS in particular does NOT put it on PATH by default,
/// so we check the well-known app-bundle path explicitly.
pub fn find_code_cli() -> Option<String> {
    // 1. On PATH
    for name in ["code", "code-insiders", "codium"] {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    // 2. Known absolute locations per OS
    let candidates: Vec<PathBuf> = match Os::current() {
        Os::Macos => vec![
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code".into(),
            "/Applications/Visual Studio Code - Insiders.app/Contents/Resources/app/bin/code"
                .into(),
        ],
        Os::Linux => vec![
            "/usr/share/code/bin/code".into(),
            "/usr/bin/code".into(),
            "/snap/bin/code".into(),
            "/var/lib/flatpak/exports/bin/com.visualstudio.code".into(),
            "/opt/visual-studio-code/bin/code".into(),
        ],
        Os::Windows => {
            let mut v = Vec::new();
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                v.push(
                    PathBuf::from(&local)
                        .join("Programs")
                        .join("Microsoft VS Code")
                        .join("bin")
                        .join("code.cmd"),
                );
            }
            if let Ok(pf) = std::env::var("ProgramFiles") {
                v.push(
                    PathBuf::from(&pf)
                        .join("Microsoft VS Code")
                        .join("bin")
                        .join("code.cmd"),
                );
            }
            v
        }
    };
    candidates
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

/// `rel` with `/` separators appended to `root` as real path components.
///
/// `Path::join("opt/homebrew/bin/brew")` appends the argument verbatim, so the slashes
/// survive into the resulting path. On Windows that string differs from one built
/// component-by-component (`\`), which is why paths expected in tests and paths produced
/// by lookups must be assembled the same way.
fn under(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |acc, part| acc.join(part))
}

/// Minimal `which` — avoids pulling a crate for one function.
pub fn which(name: &str) -> Option<String> {
    which_in(&search_path(), name, Os::current())
}

fn which_in(path: &OsStr, name: &str, os: Os) -> Option<String> {
    let names = candidate_names(name, os);
    std::env::split_paths(path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().to_string())
}

/// The file names `name` can have on disk. Windows only runs `.exe`, `.cmd` and `.bat`: a
/// bare `code` there is VS Code's shell script (meant for WSL), which CreateProcess rejects
/// as "not a valid Win32 application".
fn candidate_names(name: &str, os: Os) -> Vec<String> {
    match os {
        Os::Windows => ["exe", "cmd", "bat"]
            .iter()
            .map(|ext| format!("{name}.{ext}"))
            .collect(),
        Os::Macos | Os::Linux => vec![name.to_string()],
    }
}

fn version_of(program: &str, args: &[&str]) -> Option<String> {
    let out = command(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(s.lines().next().unwrap_or("").to_string())
}

pub fn probe(tool: Tool) -> Probe {
    match tool {
        Tool::Vscode => {
            let path = find_code_cli();
            let version = path.as_ref().and_then(|p| version_of(p, &["--version"]));
            Probe {
                found: path.is_some(),
                version,
                path,
                tool,
                too_old: None,
            }
        }
        Tool::Git => {
            let path = which("git");
            let version = path.as_ref().and_then(|p| version_of(p, &["--version"]));
            Probe {
                found: path.is_some(),
                version,
                path,
                tool,
                too_old: None,
            }
        }
        Tool::Node => {
            let path = which("node");
            let version = path.as_ref().and_then(|p| version_of(p, &["--version"]));
            let too_old = version.as_deref().and_then(|v| {
                major_version(v).filter(|m| *m < MIN_NODE_MAJOR).map(|m| {
                    format!(
                        "Node {m} is too old for the class kit (needs {MIN_NODE_MAJOR} or newer)."
                    )
                })
            });
            Probe {
                found: path.is_some(),
                version,
                path,
                tool,
                too_old,
            }
        }
    }
}

/// Which Linux distro family, for picking the right package manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinuxFamily {
    Arch,
    Debian,
    Fedora,
    Other,
}

pub fn linux_family() -> LinuxFamily {
    let id = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .to_lowercase();
    if which("pacman").is_some() || id.contains("arch") || id.contains("endeavouros") {
        LinuxFamily::Arch
    } else if which("apt-get").is_some() || id.contains("debian") || id.contains("ubuntu") {
        LinuxFamily::Debian
    } else if which("dnf").is_some() || id.contains("fedora") {
        LinuxFamily::Fedora
    } else {
        LinuxFamily::Other
    }
}

/// Build the install command for a tool. `None` = we have no automated path and the
/// UI must show manual instructions instead of failing silently.
pub fn install_command(tool: Tool) -> Option<CmdSpec> {
    match Os::current() {
        Os::Windows => {
            let id = match tool {
                Tool::Vscode => "Microsoft.VisualStudioCode",
                Tool::Git => "Git.Git",
                Tool::Node => "OpenJS.NodeJS.LTS",
            };
            Some(
                CmdSpec::new(
                    "winget",
                    &[
                        "install",
                        "--id",
                        id,
                        "-e",
                        "--accept-source-agreements",
                        "--accept-package-agreements",
                        "--silent",
                    ],
                )
                .also_ok_on(&WINGET_NOTHING_TO_DO),
            )
        }
        Os::Macos => {
            let formula = match tool {
                Tool::Vscode => "visual-studio-code",
                Tool::Git => "git",
                Tool::Node => "node",
            };
            // Homebrew is the only sane automated path on macOS. Absolute path, since
            // PATH won't have it; if it's missing, `ensure_brew` installs it to the
            // default location before this command runs.
            let brew = find_brew().unwrap_or_else(|| default_brew_path().to_string());
            let mut args = vec!["install"];
            if tool == Tool::Vscode {
                args.push("--cask");
            }
            args.push(formula);
            Some(CmdSpec::new(&brew, &args))
        }
        Os::Linux => match linux_family() {
            LinuxFamily::Arch => Some(CmdSpec::new(
                "sudo",
                &["pacman", "-S", "--needed", "--noconfirm", arch_pkg(tool)],
            )),
            LinuxFamily::Debian => Some(CmdSpec::new(
                "sudo",
                &["apt-get", "install", "-y", debian_pkg(tool)],
            )),
            LinuxFamily::Fedora => Some(CmdSpec::new(
                "sudo",
                &["dnf", "install", "-y", fedora_pkg(tool)],
            )),
            LinuxFamily::Other => None,
        },
    }
}

fn arch_pkg(tool: Tool) -> &'static str {
    match tool {
        Tool::Vscode => "code",
        Tool::Git => "git",
        Tool::Node => "nodejs",
    }
}
fn debian_pkg(tool: Tool) -> &'static str {
    match tool {
        // `code` is not in Debian main; document the manual path in the UI.
        Tool::Vscode => "code",
        Tool::Git => "git",
        Tool::Node => "nodejs",
    }
}
fn fedora_pkg(tool: Tool) -> &'static str {
    match tool {
        Tool::Vscode => "code",
        Tool::Git => "git",
        Tool::Node => "nodejs",
    }
}

/// Manual instructions shown when we cannot automate (unknown distro, no winget, or the
/// Homebrew auto-install failed — the caller prepends the specific reason).
pub fn manual_instructions(tool: Tool) -> Vec<String> {
    match (Os::current(), tool) {
        (Os::Windows, Tool::Vscode) => vec![
            "winget not available. Install 'App Installer' from the Microsoft Store, or download VS Code:".into(),
            "https://code.visualstudio.com/download".into(),
        ],
        (Os::Windows, Tool::Git) => vec![
            "winget not available. Download Git for Windows:".into(),
            "https://git-scm.com/download/win".into(),
        ],
        (Os::Windows, Tool::Node) => vec![
            "winget not available. Download Node.js LTS:".into(),
            "https://nodejs.org/en/download".into(),
        ],
        (Os::Macos, _) => vec![
            "Install Homebrew yourself in Terminal, then re-run this wizard:".into(),
            "https://brew.sh".into(),
        ],
        (Os::Linux, Tool::Vscode) => vec![
            "Could not detect your package manager. Install VS Code manually:".into(),
            "https://code.visualstudio.com/docs/setup/linux".into(),
        ],
        (Os::Linux, Tool::Git) => vec![
            "Install git with your distro's package manager.".into(),
            "e.g. pacman -S git  |  apt install git  |  dnf install git".into(),
        ],
        (Os::Linux, Tool::Node) => vec![
            "Install Node.js LTS with your distro's package manager.".into(),
            "e.g. pacman -S nodejs npm  |  apt install nodejs npm".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_current_is_known() {
        let os = Os::current();
        assert!(matches!(os, Os::Windows | Os::Macos | Os::Linux));
    }

    #[test]
    fn cmdspec_display_formats() {
        let c = CmdSpec::new("brew", &["install", "git"]);
        assert_eq!(c.display, "brew install git");
        assert_eq!(c.program, "brew");
        assert_eq!(c.args, vec!["install", "git"]);
    }

    #[test]
    fn cmdspec_display_without_args() {
        let c = CmdSpec::new("git", &[]);
        assert_eq!(c.display, "git");
    }

    #[test]
    fn install_command_exists_for_known_platforms() {
        // Linux/Windows always have one; macOS does too now, since a missing Homebrew is
        // installed on the fly rather than reported as manual.
        for t in [Tool::Vscode, Tool::Git, Tool::Node] {
            assert!(
                install_command(t).is_some(),
                "{t:?} should have an install command"
            );
        }
    }

    /// A throwaway dir tree under the OS temp dir, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("abs-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }

        /// `rel` is written with `/` for readability and assembled component-by-component by
        /// `under`, so the result matches paths the production lookups build on any platform.
        fn touch(&self, rel: &str) -> PathBuf {
            let p = under(&self.0, rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            p
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn find_brew_finds_apple_silicon_location() {
        let root = TempRoot::new("brew-arm");
        let brew = root.touch("opt/homebrew/bin/brew");
        let found = find_brew_under(&root.0, |_| None);
        assert_eq!(found, Some(brew.to_string_lossy().to_string()));
    }

    #[test]
    fn find_brew_prefers_apple_silicon_over_intel() {
        let root = TempRoot::new("brew-both");
        let arm = root.touch("opt/homebrew/bin/brew");
        root.touch("usr/local/bin/brew");
        let found = find_brew_under(&root.0, |_| None);
        assert_eq!(found, Some(arm.to_string_lossy().to_string()));
    }

    #[test]
    fn find_brew_falls_back_to_intel_then_which() {
        let root = TempRoot::new("brew-intel");
        assert_eq!(
            find_brew_under(&root.0, |n| Some(format!("/custom/{n}"))),
            Some("/custom/brew".to_string()),
            "nothing at known locations should defer to which()"
        );
        let intel = root.touch("usr/local/bin/brew");
        assert_eq!(
            find_brew_under(&root.0, |_| panic!(
                "which() must not run when brew is found"
            )),
            Some(intel.to_string_lossy().to_string())
        );
    }

    #[test]
    fn find_brew_none_when_absent() {
        let root = TempRoot::new("brew-none");
        assert_eq!(find_brew_under(&root.0, |_| None), None);
    }

    #[test]
    fn macos_well_known_dirs_include_homebrew() {
        let dirs = well_known_bin_dirs_for(Os::Macos);
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    #[test]
    fn linux_has_no_well_known_dirs() {
        assert!(well_known_bin_dirs_for(Os::Linux).is_empty());
    }

    #[test]
    fn windows_install_dirs_cover_git_node_and_vscode() {
        let dirs = windows_install_dirs(Some("PF"), Some("PF86"), Some("LOCAL"));
        for expected in [
            Path::new("PF").join("Git").join("cmd"),
            Path::new("PF").join("nodejs"),
            Path::new("PF").join("Microsoft VS Code").join("bin"),
            Path::new("PF86").join("nodejs"),
            Path::new("LOCAL")
                .join("Programs")
                .join("Microsoft VS Code")
                .join("bin"),
            Path::new("LOCAL").join("Programs").join("Git").join("cmd"),
        ] {
            assert!(dirs.contains(&expected), "missing {expected:?}");
        }
    }

    #[test]
    fn windows_install_dirs_skip_unset_roots() {
        let dirs = windows_install_dirs(Some("PF"), None, None);
        assert!(!dirs.is_empty());
        assert!(dirs.iter().all(|d| d.starts_with("PF")), "{dirs:?}");
        assert!(windows_install_dirs(None, None, None).is_empty());
    }

    fn fake_env(key: &str) -> Option<String> {
        match key {
            "SystemRoot" => Some("WIN".into()),
            "USERPROFILE" => Some("HOME".into()),
            _ => None,
        }
    }

    #[test]
    fn expand_env_vars_substitutes_known_variables() {
        assert_eq!(expand_env_vars("%SystemRoot%", &fake_env), "WIN");
        assert_eq!(
            expand_env_vars("%USERPROFILE%\\bin;%SystemRoot%", &fake_env),
            "HOME\\bin;WIN"
        );
    }

    #[test]
    fn expand_env_vars_leaves_unknown_and_unclosed_references() {
        assert_eq!(expand_env_vars("%NOPE%\\x", &fake_env), "%NOPE%\\x");
        assert_eq!(expand_env_vars("100%", &fake_env), "100%");
        assert_eq!(expand_env_vars("plain", &fake_env), "plain");
    }

    #[test]
    fn path_entries_splits_expands_and_drops_blanks() {
        let values = vec![
            "%SystemRoot%\\system32;;C:\\Tools ".to_string(),
            "%USERPROFILE%\\AppData".to_string(),
        ];
        assert_eq!(
            path_entries(&values, fake_env),
            vec![
                PathBuf::from("WIN\\system32"),
                PathBuf::from("C:\\Tools"),
                PathBuf::from("HOME\\AppData"),
            ]
        );
    }

    #[test]
    fn windows_well_known_dirs_are_never_a_panic_on_any_host() {
        // The registry half only exists on Windows; elsewhere this is just the env-var dirs.
        let _ = well_known_bin_dirs_for(Os::Windows);
    }

    #[test]
    fn which_on_windows_ignores_the_extensionless_script() {
        let root = TempRoot::new("which-win");
        root.touch("bin/code"); // VS Code's shell script, meant for WSL
        let cmd = root.touch("bin/code.cmd");
        let path = std::env::join_paths([root.0.join("bin")]).unwrap();
        assert_eq!(
            which_in(&path, "code", Os::Windows),
            Some(cmd.to_string_lossy().to_string())
        );
    }

    #[test]
    fn which_on_windows_rejects_a_lone_extensionless_file() {
        let root = TempRoot::new("which-win-bare");
        root.touch("bin/code");
        let path = std::env::join_paths([root.0.join("bin")]).unwrap();
        assert_eq!(which_in(&path, "code", Os::Windows), None);
    }

    #[test]
    fn which_on_windows_accepts_exe_cmd_and_bat() {
        let root = TempRoot::new("which-win-ext");
        let bat = root.touch("bin/legacy.bat");
        let exe = root.touch("bin/git.exe");
        let path = std::env::join_paths([root.0.join("bin")]).unwrap();
        assert_eq!(
            which_in(&path, "legacy", Os::Windows),
            Some(bat.to_string_lossy().to_string())
        );
        assert_eq!(
            which_in(&path, "git", Os::Windows),
            Some(exe.to_string_lossy().to_string())
        );
    }

    #[test]
    fn which_elsewhere_takes_the_bare_name_and_first_dir_wins() {
        let root = TempRoot::new("which-unix");
        let first = root.touch("a/tool");
        root.touch("b/tool");
        let path = std::env::join_paths([root.0.join("a"), root.0.join("b")]).unwrap();
        assert_eq!(
            which_in(&path, "tool", Os::Linux),
            Some(first.to_string_lossy().to_string())
        );
        assert_eq!(which_in(&path, "missing", Os::Linux), None);
    }

    #[test]
    fn winget_nothing_to_do_codes_match_the_published_values() {
        assert!(WINGET_NOTHING_TO_DO.contains(&-1978335189)); // 0x8A15002B
        assert!(WINGET_NOTHING_TO_DO.contains(&-1978335135)); // 0x8A150061
        assert!(WINGET_NOTHING_TO_DO.contains(&-1978334963)); // 0x8A15010D
    }

    #[test]
    fn winget_install_command_tolerates_already_installed() {
        if Os::current() != Os::Windows {
            return;
        }
        let c = install_command(Tool::Git).unwrap();
        assert_eq!(c.ok_exit_codes, WINGET_NOTHING_TO_DO);
    }

    fn collect(bytes: &[&[u8]]) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut splitter = LineSplitter::default();
        let mut emit = |seg: Vec<u8>, redrawn: bool| {
            out.push((String::from_utf8(seg).unwrap(), redrawn));
        };
        for chunk in bytes {
            splitter.push(chunk, &mut emit);
        }
        splitter.finish(&mut emit);
        out
    }

    #[test]
    fn splitter_treats_crlf_as_one_ordinary_line_ending() {
        assert_eq!(
            collect(&[b"one\r\ntwo\r\n"]),
            [("one".into(), false), ("two".into(), false)]
        );
    }

    #[test]
    fn splitter_marks_bare_cr_segments_as_redrawn() {
        assert_eq!(
            collect(&[b"10%\r20%\r30%\ndone\n"]),
            [
                ("10%".into(), true),
                ("20%".into(), true),
                ("30%".into(), false),
                ("done".into(), false),
            ]
        );
    }

    #[test]
    fn splitter_resolves_cr_lf_split_across_reads() {
        assert_eq!(
            collect(&[b"line\r", b"\nnext"]),
            [("line".into(), false), ("next".into(), false)]
        );
    }

    #[test]
    fn splitter_flushes_a_final_unterminated_line() {
        assert_eq!(collect(&[b"no newline"]), [("no newline".into(), false)]);
        assert_eq!(collect(&[b"bar\r"]), [("bar".into(), true)]);
    }

    #[test]
    fn splitter_keeps_multibyte_characters_split_across_reads() {
        let bytes = "é\n".as_bytes();
        assert_eq!(collect(&[&bytes[..1], &bytes[1..]]), [("é".into(), false)]);
    }

    #[test]
    fn clean_line_strips_ansi_escapes() {
        assert_eq!(
            clean_line("\x1b[2K\x1b[32mDownloading\x1b[0m"),
            Some("Downloading".into())
        );
        assert_eq!(
            clean_line("\x1b]0;title\x07Installing"),
            Some("Installing".into())
        );
        assert_eq!(clean_line("\x1b[?25lHello\x1b[?25h"), Some("Hello".into()));
    }

    #[test]
    fn clean_line_drops_spinner_and_bar_only_lines() {
        for junk in [
            "-",
            "\\",
            "|",
            "/",
            "   ",
            "",
            "\u{2588}\u{2588}\u{2592}\u{2592}",
            "\x1b[1A",
        ] {
            assert_eq!(clean_line(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn clean_line_keeps_the_text_around_a_progress_bar() {
        assert_eq!(
            clean_line("  \u{2588}\u{2588}\u{2588}\u{2592}\u{2592}  45%"),
            Some("45%".into())
        );
        assert_eq!(
            clean_line("  7.00 MB / 25.0 MB"),
            Some("7.00 MB / 25.0 MB".into())
        );
    }

    #[cfg(unix)]
    fn sh(script: &str) -> CmdSpec {
        CmdSpec::new("/bin/sh", &["-c", script])
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_reports_lines_and_flags_redraws() {
        let mut seen: Vec<(String, bool)> = Vec::new();
        let out = sh("printf 'start\\n'; printf '10%%\\r50%%\\r'; printf 'end\\n'; echo oops >&2")
            .run_streaming(&mut |l, t| seen.push((l.to_string(), t)))
            .unwrap();
        assert!(seen.contains(&("start".into(), false)));
        assert!(seen.contains(&("10%".into(), true)));
        assert!(seen.contains(&("50%".into(), true)));
        assert!(seen.contains(&("end".into(), false)));
        assert!(seen.contains(&("oops".into(), false)));
        assert_eq!(out, "start\nend", "redraws stay out of the returned output");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_delivers_lines_while_the_child_is_still_running() {
        let started = std::time::Instant::now();
        let mut first_seen = None;
        sh("echo first; sleep 1; echo second")
            .run_streaming(&mut |line, _| {
                if line == "first" {
                    first_seen = Some(started.elapsed());
                }
            })
            .unwrap();
        let total = started.elapsed();
        let first_seen = first_seen.expect("first line was never delivered");
        assert!(
            total - first_seen > std::time::Duration::from_millis(600),
            "first line arrived at {first_seen:?} but the run only ended at {total:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_failure_carries_stderr_without_progress_noise() {
        let err = sh("printf '10%%\\r'; echo boom >&2; exit 3")
            .run()
            .unwrap_err();
        assert!(err.ends_with("failed: boom"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_treats_listed_exit_codes_as_success() {
        assert!(sh("exit 7").run().is_err());
        assert!(sh("exit 7").also_ok_on(&[7]).run().is_ok());
        assert!(sh("exit 8").also_ok_on(&[7]).run().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_does_not_deadlock_on_a_flood_of_stderr() {
        let spec =
            sh("i=0; while [ $i -lt 20000 ]; do echo err-$i >&2; i=$((i+1)); done; echo done");
        assert_eq!(spec.run().unwrap(), "done");
    }

    #[test]
    fn run_streaming_reports_a_missing_program() {
        let err = CmdSpec::new("definitely-not-a-real-program-xyz", &[])
            .run()
            .unwrap_err();
        assert!(err.contains("failed to run"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn command_passes_a_corrected_path_when_it_differs() {
        // Build the dirs with the host separator so this holds on any Unix.
        let out = command("/bin/sh")
            .args(["-c", "printf %s \"$PATH\""])
            .output()
            .unwrap();
        let child_path = OsString::from(String::from_utf8_lossy(&out.stdout).into_owned());
        assert_eq!(child_path, search_path());
    }

    #[test]
    fn path_with_dirs_prepends_homebrew_to_launchd_path() {
        // Build the launchd PATH with the HOST separator, so this test asserts the same
        // thing on Windows (where `;` is the separator and `/usr/bin` means nothing).
        let bare_dirs = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
        let bare = std::env::join_paths(bare_dirs).unwrap();
        let extra = well_known_bin_dirs_for(Os::Macos);
        let out = path_with_dirs(Some(bare), &extra);
        let parts: Vec<PathBuf> = std::env::split_paths(&out).collect();

        // Homebrew is prepended so its tools win over the system shims.
        assert_eq!(parts[0], PathBuf::from("/opt/homebrew/bin"));
        // ...and every original entry survives.
        for d in bare_dirs {
            assert!(
                parts.contains(&PathBuf::from(d)),
                "original entry {d} was dropped"
            );
        }
    }

    #[test]
    fn path_with_dirs_does_not_duplicate_existing_entries() {
        let existing = std::env::join_paths(["/usr/bin", "/opt/homebrew/bin"]).unwrap();
        let extra = vec![PathBuf::from("/opt/homebrew/bin")];
        let out = path_with_dirs(Some(existing), &extra);
        let count = std::env::split_paths(&out)
            .filter(|d| d == Path::new("/opt/homebrew/bin"))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn path_with_dirs_is_identity_with_nothing_to_add() {
        let existing = OsString::from("/usr/bin:/bin");
        assert_eq!(path_with_dirs(Some(existing.clone()), &[]), existing);
        assert_eq!(path_with_dirs(None, &[]), OsString::new());
    }

    #[test]
    fn search_path_keeps_every_process_path_entry() {
        let out = search_path();
        let have: Vec<PathBuf> = std::env::split_paths(&out).collect();
        for d in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            assert!(have.contains(&d), "{d:?} was dropped from PATH");
        }
    }

    #[test]
    fn brew_installer_spec_is_well_formed() {
        let c = brew_installer_spec();
        assert_eq!(c.program, "/bin/bash");
        assert_eq!(c.args.len(), 2);
        assert_eq!(c.args[0], "-c");
        assert!(c.args[1].starts_with("NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL "));
        assert!(c.args[1]
            .ends_with("https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""));
        assert!(
            !c.args[1].contains("  "),
            "line continuation left stray spaces"
        );
        assert!(c.display.starts_with("/bin/bash -c NONINTERACTIVE=1"));
    }

    #[test]
    fn brew_bootstrap_command_only_when_brew_missing_on_macos() {
        let needed = brew_bootstrap_command().is_some();
        assert_eq!(needed, Os::current() == Os::Macos && find_brew().is_none());
    }

    #[test]
    fn brew_install_error_explains_the_cause() {
        assert!(brew_install_error("Need sudo access on macOS").contains("administrator password"));
        assert!(
            brew_install_error("sudo: a terminal is required to read the password")
                .contains("administrator password")
        );
        assert!(brew_install_error("curl: (6) Could not resolve host").contains("internet"));
        assert!(brew_install_error("???").contains("couldn't be installed"));
    }

    #[test]
    fn macos_brew_install_command_uses_absolute_path() {
        if Os::current() != Os::Macos {
            return;
        }
        let c = install_command(Tool::Node).expect("macOS always has a brew command");
        assert!(c.program.ends_with("/brew"), "got {}", c.program);
        assert!(Path::new(&c.program).is_absolute());
        assert_eq!(c.args, vec!["install", "node"]);
        let code = install_command(Tool::Vscode).unwrap();
        assert_eq!(code.args, vec!["install", "--cask", "visual-studio-code"]);
    }

    #[test]
    fn major_version_parses_the_shapes_node_actually_prints() {
        assert_eq!(major_version("v20.11.1"), Some(20));
        assert_eq!(major_version("v18.19.0"), Some(18));
        assert_eq!(major_version("20.11.1"), Some(20));
        assert_eq!(major_version("v22.0.0-pre"), Some(22));
        assert_eq!(major_version("  v25.9.0  "), Some(25));
        assert_eq!(major_version("v8.3.0"), Some(8));
        assert_eq!(major_version("nonsense"), None);
        assert_eq!(major_version(""), None);
    }

    /// The bug this guards: an installed Node 18 used to report "already installed" and get
    /// skipped, so the student hit the failure in class instead of at setup.
    #[test]
    fn node_18_is_flagged_too_old_and_node_20_is_not() {
        assert!(major_version("v18.19.0").unwrap() < MIN_NODE_MAJOR);
        assert!(major_version("v20.11.1").unwrap() >= MIN_NODE_MAJOR);
        assert!(major_version("v25.9.0").unwrap() >= MIN_NODE_MAJOR);
        assert_eq!(MIN_NODE_MAJOR, 20);
    }

    #[test]
    fn tool_labels_are_stable() {
        assert_eq!(Tool::Vscode.label(), "Visual Studio Code");
        assert_eq!(Tool::Git.label(), "Git");
        assert_eq!(Tool::Node.label(), "Node.js LTS");
    }
}
