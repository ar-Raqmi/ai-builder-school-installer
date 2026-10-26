//! The wizard's step orchestration — exposed to the UI as Tauri commands.

use crate::api;
use crate::kilo;
use crate::platform::{self, CmdSpec, Os, Probe, Tool};
use serde::Serialize;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const KILO_EXTENSION_ID: &str = "kilocode.Kilo-Code";

#[derive(Debug, Clone, Serialize)]
pub struct StepState {
    pub id: String,
    pub label: String,
    /// pending | running | done | skipped | failed | manual
    pub status: String,
    pub detail: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectionReport {
    pub os: String,
    pub steps: Vec<StepState>,
}

/// Event the UI listens to for live output from the step that is running.
const PROGRESS_EVENT: &str = "install-progress";

/// Redrawn lines (progress bars) can arrive hundreds of times a second; the webview only
/// needs a few a second to look alive.
const TRANSIENT_EVERY: Duration = Duration::from_millis(100);

#[derive(Clone, Serialize)]
struct ProgressLine<'a> {
    id: &'a str,
    line: &'a str,
    /// Redrawn in place by the tool: show it under the step, don't keep it in the log.
    transient: bool,
}

/// Lets an event through at most once per `every`.
struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    fn ready(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < self.every)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Forwards one step's output to the UI as `install-progress` events.
struct ProgressEmitter {
    app: AppHandle,
    id: String,
    throttle: Throttle,
}

impl ProgressEmitter {
    fn new(app: AppHandle, id: &str) -> Self {
        ProgressEmitter {
            app,
            id: id.to_string(),
            throttle: Throttle {
                every: TRANSIENT_EVERY,
                last: None,
            },
        }
    }

    /// Real log lines always go through; only redraws are thinned out.
    fn emit(&mut self, line: &str, transient: bool) {
        if transient && !self.throttle.ready(Instant::now()) {
            return;
        }
        let payload = ProgressLine {
            id: &self.id,
            line,
            transient,
        };
        let _ = self.app.emit(PROGRESS_EVENT, payload);
    }
}

fn os_label() -> String {
    match Os::current() {
        Os::Windows => "Windows".into(),
        Os::Macos => "macOS".into(),
        Os::Linux => format!("Linux ({:?})", platform::linux_family()),
    }
}

/// Probe all three tools and report what would happen. Read-only; safe to call on start.
#[tauri::command(async)]
pub fn detect() -> DetectionReport {
    let mut steps = Vec::new();
    for tool in [Tool::Vscode, Tool::Git, Tool::Node] {
        let p: Probe = platform::probe(tool);
        let cmd = platform::install_command(tool);
        let (status, detail, command) = if p.found && p.too_old.is_none() {
            (
                "skipped".to_string(),
                format!(
                    "already installed{}",
                    p.version
                        .as_deref()
                        .map(|v| format!(" — {}", first_line(v)))
                        .unwrap_or_default()
                ),
                None,
            )
        } else if p.found {
            // Present but below the version the kit needs: offer the upgrade rather than
            // skipping, so a stale Node can't sail through as "already installed".
            (
                "pending".to_string(),
                p.too_old.clone().unwrap_or_default(),
                cmd.as_ref().map(|c| c.display.clone()),
            )
        } else if let Some(c) = &cmd {
            (
                "pending".to_string(),
                if platform::brew_bootstrap_command().is_some() {
                    "needs installing (Homebrew first)".to_string()
                } else {
                    "needs installing".to_string()
                },
                Some(c.display.clone()),
            )
        } else {
            (
                "manual".to_string(),
                platform::manual_instructions(tool).join(" "),
                None,
            )
        };
        steps.push(StepState {
            id: tool_key(tool).into(),
            label: tool.label().into(),
            status,
            detail,
            command,
        });
    }

    // Kilo Code extension
    let code = platform::find_code_cli();
    let ext_status = if code.is_none() {
        ("pending", "waiting for VS Code")
    } else {
        ("pending", "will install")
    };
    steps.push(StepState {
        id: "kilo".into(),
        label: "Kilo Code".into(),
        status: ext_status.0.into(),
        detail: ext_status.1.into(),
        command: code
            .as_ref()
            .map(|c| format!("{} --install-extension {KILO_EXTENSION_ID} --force", c)),
    });

    steps.push(StepState {
        id: "api".into(),
        label: "API key".into(),
        status: "pending".into(),
        detail: kilo::BASE_URL.into(),
        command: None,
    });

    DetectionReport {
        os: os_label(),
        steps,
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn tool_key(t: Tool) -> &'static str {
    match t {
        Tool::Vscode => "vscode",
        Tool::Git => "git",
        Tool::Node => "node",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub ok: bool,
    pub status: String,
    pub detail: String,
    pub command: Option<String>,
}

/// Execute the install command for one tool, then re-probe.
///
/// `async` only moves this onto a worker thread: a plain command runs on the main thread,
/// where a multi-minute winget/brew run would freeze the window.
#[tauri::command(async)]
pub fn install_tool(app: AppHandle, id: String, dry_run: bool) -> RunResult {
    let mut progress = ProgressEmitter::new(app, &id);
    install_tool_streaming(&id, dry_run, &mut |line, t| progress.emit(line, t))
}

/// `install_tool` with the output delivered to `on_line(line, transient)`, so it can run
/// without a window.
pub fn install_tool_streaming(
    id: &str,
    dry_run: bool,
    on_line: &mut dyn FnMut(&str, bool),
) -> RunResult {
    let tool = match id {
        "vscode" => Tool::Vscode,
        "git" => Tool::Git,
        "node" => Tool::Node,
        other => {
            return RunResult {
                ok: false,
                status: "failed".into(),
                detail: format!("unknown tool `{other}`"),
                command: None,
            }
        }
    };

    let probed = platform::probe(tool);
    // A present-but-old Node must still be upgraded, or the student reaches class with a
    // runtime the kit can't use.
    if probed.found && probed.too_old.is_none() {
        return RunResult {
            ok: true,
            status: "skipped".into(),
            detail: "already installed".into(),
            command: None,
        };
    }

    let cmd: CmdSpec = match platform::install_command(tool) {
        Some(c) => c,
        None => {
            return RunResult {
                ok: false,
                status: "manual".into(),
                detail: platform::manual_instructions(tool).join(" "),
                command: None,
            }
        }
    };

    let bootstrap = platform::brew_bootstrap_command();

    if dry_run {
        let steps = match &bootstrap {
            Some(b) => format!("{} && {}", b.display, cmd.display),
            None => cmd.display.clone(),
        };
        return RunResult {
            ok: true,
            status: "pending".into(),
            detail: format!("[dry-run] would run: {steps}"),
            command: Some(cmd.display),
        };
    }

    // A fresh Mac has no Homebrew. Install it first; if that can't be done unattended,
    // say why and fall back to manual rather than failing opaquely.
    let cmd = if bootstrap.is_some() {
        if let Err(reason) = platform::ensure_brew(on_line) {
            return RunResult {
                ok: false,
                status: "manual".into(),
                detail: format!("{reason} {}", platform::manual_instructions(tool).join(" ")),
                command: bootstrap.map(|b| b.display),
            };
        }
        // brew now exists, possibly at a different path than the one guessed earlier.
        platform::install_command(tool).unwrap_or(cmd)
    } else {
        cmd
    };

    match cmd.run_streaming(on_line) {
        Ok(_) => {
            let after = platform::probe(tool);
            RunResult {
                ok: after.found,
                status: if after.found { "done" } else { "failed" }.into(),
                detail: if after.found {
                    format!(
                        "installed{}",
                        after
                            .version
                            .as_deref()
                            .map(|v| format!(" — {}", first_line(v)))
                            .unwrap_or_default()
                    )
                } else {
                    "command succeeded but the tool was not detected afterwards".into()
                },
                command: Some(cmd.display),
            }
        }
        Err(e) => RunResult {
            ok: false,
            status: "failed".into(),
            detail: e,
            command: Some(cmd.display),
        },
    }
}

/// Install the Kilo Code extension.
#[tauri::command(async)]
pub fn install_extension(app: AppHandle, dry_run: bool) -> RunResult {
    let mut progress = ProgressEmitter::new(app, "kilo");
    install_extension_streaming(dry_run, &mut |line, t| progress.emit(line, t))
}

/// `install_extension` with the output delivered to `on_line(line, transient)`.
pub fn install_extension_streaming(
    dry_run: bool,
    on_line: &mut dyn FnMut(&str, bool),
) -> RunResult {
    let code = match platform::find_code_cli() {
        Some(c) => c,
        None => {
            return RunResult {
                ok: false,
                status: "failed".into(),
                detail: "VS Code was not found, so the extension cannot be installed yet.".into(),
                command: None,
            }
        }
    };
    let cmd = CmdSpec::new(
        &code,
        &["--install-extension", KILO_EXTENSION_ID, "--force"],
    );
    if dry_run {
        return RunResult {
            ok: true,
            status: "pending".into(),
            detail: format!("[dry-run] would run: {}", cmd.display),
            command: Some(cmd.display),
        };
    }
    match cmd.run_streaming(on_line) {
        Ok(_) => RunResult {
            ok: true,
            status: "done".into(),
            detail: "Kilo Code installed".into(),
            command: Some(cmd.display),
        },
        Err(e) => RunResult {
            ok: false,
            status: "failed".into(),
            detail: e,
            command: Some(cmd.display),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyOutcome {
    pub ok: bool,
    pub models: Vec<String>,
    pub default_model: String,
    pub message: String,
    pub latency_ms: u128,
}

/// PING the gateway with the supplied key and return the model list.
#[tauri::command(async)]
pub fn verify_key(
    api_key: String,
    base_url: Option<String>,
    preferred: Option<String>,
) -> VerifyOutcome {
    let base = base_url.unwrap_or_else(|| kilo::BASE_URL.to_string());
    match api::verify_and_list(&base, &api_key) {
        Ok(r) => {
            let default_model = api::choose_default(&r.models, preferred.as_deref())
                .unwrap_or_else(|| api::DEFAULT_MODEL.to_string());
            VerifyOutcome {
                ok: true,
                models: r.models,
                default_model,
                message: r.message,
                latency_ms: r.latency_ms,
            }
        }
        Err(e) => VerifyOutcome {
            ok: false,
            models: vec![],
            default_model: String::new(),
            message: e,
            latency_ms: 0,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub ok: bool,
    pub log: Vec<String>,
    pub fatal: Option<String>,
}

/// Write the Kilo config + credentials. `api_key` may be empty when the student
/// chooses to skip and use Kilo's own free models.
#[tauri::command(async)]
pub fn apply_config(
    api_key: Option<String>,
    models: Vec<String>,
    default_model: Option<String>,
    use_env: bool,
    dry_run: bool,
) -> ApplyOutcome {
    let mut log = Vec::new();
    let key = api_key.unwrap_or_default();
    let skip = key.trim().is_empty();

    if skip {
        log.push("No API key supplied — skipping provider setup. Open Kilo Code and sign in to use its free models.".into());
        return ApplyOutcome {
            ok: true,
            log,
            fatal: None,
        };
    }

    if dry_run {
        log.push(format!(
            "[dry-run] would write {} provider config to {}",
            kilo::PROVIDER_NAME,
            kilo::kilo_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ));
        return ApplyOutcome {
            ok: true,
            log,
            fatal: None,
        };
    }

    let existing = match kilo::load_existing() {
        Ok(v) => v,
        Err(e) => {
            return ApplyOutcome {
                ok: false,
                log,
                fatal: Some(format!(
                    "Existing Kilo config could not be read ({e}). Back it up and re-run."
                )),
            }
        }
    };

    let default = default_model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| api::DEFAULT_MODEL.to_string());

    let cfg = kilo::build_config(&existing, &models, &default, &key, use_env);
    match kilo::write_config(&cfg) {
        Ok(p) => log.push(format!("Wrote {}", p.display())),
        Err(e) => {
            return ApplyOutcome {
                ok: false,
                log,
                fatal: Some(format!("Could not write the Kilo config: {e}")),
            }
        }
    }

    match kilo::write_auth(&key) {
        Ok(p) => log.push(format!("Stored credential in {}", p.display())),
        Err(e) => log.push(format!(
            "Warning: could not update the credential store: {e}"
        )),
    }

    if use_env {
        match kilo::persist_env_var(&key) {
            Ok(msg) => log.push(msg),
            Err(e) => log.push(format!(
                "Warning: could not persist the environment variable: {e}"
            )),
        }
    }

    log.push(format!(
        "Default model set to {}/{}",
        kilo::PROVIDER_ID,
        default
    ));
    ApplyOutcome {
        ok: true,
        log,
        fatal: None,
    }
}

/// Open VS Code so the student can immediately click the Kilo Code icon.
#[tauri::command(async)]
pub fn open_vscode() -> RunResult {
    let code = match platform::find_code_cli() {
        Some(c) => c,
        None => {
            return RunResult {
                ok: false,
                status: "failed".into(),
                detail: "VS Code not found.".into(),
                command: None,
            }
        }
    };
    let cmd = CmdSpec::new(&code, &["-n"]);
    match cmd.run() {
        Ok(_) => RunResult {
            ok: true,
            status: "done".into(),
            detail: "Opened VS Code".into(),
            command: Some(cmd.display),
        },
        Err(e) => RunResult {
            ok: false,
            status: "failed".into(),
            detail: e,
            command: Some(cmd.display),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_lets_the_first_event_through() {
        let mut t = Throttle {
            every: Duration::from_millis(100),
            last: None,
        };
        assert!(t.ready(Instant::now()));
    }

    #[test]
    fn throttle_drops_events_inside_the_window_and_resumes_after() {
        let start = Instant::now();
        let mut t = Throttle {
            every: Duration::from_millis(100),
            last: None,
        };
        assert!(t.ready(start));
        assert!(!t.ready(start + Duration::from_millis(40)));
        assert!(!t.ready(start + Duration::from_millis(99)));
        assert!(t.ready(start + Duration::from_millis(100)));
        assert!(!t.ready(start + Duration::from_millis(150)));
    }

    #[test]
    fn progress_line_serialises_the_shape_the_ui_reads() {
        let json = serde_json::to_value(ProgressLine {
            id: "git",
            line: "45%",
            transient: true,
        })
        .unwrap();
        assert_eq!(json["id"], "git");
        assert_eq!(json["line"], "45%");
        assert_eq!(json["transient"], true);
    }

    #[test]
    fn unknown_tool_fails_cleanly_without_running_anything() {
        let mut lines = 0;
        let r = install_tool_streaming("nonsense", false, &mut |_, _| lines += 1);
        assert!(!r.ok);
        assert_eq!(r.status, "failed");
        assert_eq!(lines, 0);
    }
}
