# How releases are built and shipped

## The short version

**You can't build all three OSes from one machine.** That's not a Tauri flaw — it's
Apple's and Microsoft's bundler toolchains. So releases are built by **GitHub Actions on
native runners**, which is free for public repos.

Push a tag → four runners build in parallel → a draft Release appears with every
installer attached.

```bash
git tag v<version>
git push origin v<version>
```

Then go to **Actions** to watch it, and **Releases** to publish the draft.

You can also trigger it by hand: **Actions → Release → Run workflow**.

## What each platform can and cannot do

| Target | Built on | Can you cross-compile from Linux? |
|---|---|---|
| Linux `deb` / `rpm` / `AppImage` | `ubuntu-22.04` | ✅ Yes — it's the same OS |
| Windows `nsis` (`-setup.exe`) + plain `.exe` | `windows-latest` | ⚠️ Possible via `cargo-xwin`, not recommended |
| macOS `dmg` / `app` | `macos-latest` | ❌ **Impossible** — needs the Apple SDK |

Per Tauri's own docs, cross-compiling Windows from Linux *"should only be used as a last
resort if local VMs or CI solutions like GitHub Actions don't work for you."*

**macOS cannot be done at all** from Linux, because the Apple SDK isn't redistributable.
No tooling fixes that.

## What a student downloads

| OS | File | Notes |
|---|---|---|
| Windows | `aibs-setup_windows_x64.exe` | The portable exe: no "installer for the installer". `uploadPlainBinary: true` in the workflow publishes the raw `target/release` binary; tauri-action names it `<binary>_<platform>_<arch>.exe`, with no version. |
| Windows (fallback) | `AI.Builder.School.Setup_<version>_x64-setup.exe` | NSIS. Use it if the portable exe won't open: it also installs WebView2 (`webviewInstallMode: downloadBootstrapper`). |
| macOS | the Terminal one-liner (below) | Avoids Gatekeeper's prompt entirely. The `.dmg` still works but needs **Open Anyway**. |
| Linux | the Terminal one-liner (below), or the `.AppImage` | Never the `.deb`/`.rpm`; see below. |

The plain exe needs the WebView2 runtime, which ships with Windows 11 and current Windows 10.

The MSI is no longer built, so students aren't left choosing between two Windows installers.

### macOS one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/ar-Raqmi/ai-builder-school-installer/master/scripts/install-mac.sh | bash
```

`scripts/install-mac.sh` downloads the stable-named tarball
(`AI.Builder.School.Setup_{aarch64,x64}.app.tar.gz`) from `releases/latest/download/`,
unpacks it into `/Applications` (or `~/Applications` when that isn't writable; it never
uses `sudo`) and opens it. Gatekeeper only assesses files that carry the
`com.apple.quarantine` attribute. Browsers add it to downloads; `curl` does not, and `tar`
does not copy it out of an archive, so the app opens with no prompt.

It also **installs Homebrew when it's missing**, because the wizard installs VS Code and
Node through `brew` and cannot do that itself: a GUI app has no terminal, so Homebrew's
admin prompt can never be answered. The script runs the official installer with `/dev/tty`
as its stdin — piped in, the script's own stdin is the `curl` pipe, and Homebrew treats a
non-TTY stdin as non-interactive, which turns its `sudo` calls into `sudo -n` and fails on
the password prompt. If the student cancels, the app is still installed and the wizard
falls back to its manual instructions.

It reads from `master` and from the latest *published* Release, so a draft Release is
invisible to it until you publish. Because it is fetched from `master`, changes to it go
live immediately and need no new app release.

### Linux one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/ar-Raqmi/ai-builder-school-installer/master/scripts/install-linux.sh | bash
```

Unlike the macOS tarballs, Tauri puts the version in the AppImage's filename, so there is
no stable `latest/download/<name>` URL. `scripts/install-linux.sh` asks the GitHub API for
the latest release, picks the asset ending in `_<arch>.AppImage`, and saves it as
`~/.local/bin/ai-builder-school-setup` so the command students use never changes. The
script is fetched from `master`, not shipped in a bundle, so the build needs no changes
for it. It never uses `sudo`; if the machine lacks FUSE it unpacks the AppImage with
`--appimage-extract` and runs `AppRun` instead.

Like the macOS one, it only sees the latest *published* Release. The API is unauthenticated,
so a shared network can hit GitHub's rate limit; the script says so and points at the
Releases page. The workflow only builds Linux for x86_64, so on an ARM machine the script
reports that no AppImage exists yet.

## Which Linux file should a student download?

Not the `.deb`, and not the `.rpm`. Use the **`.AppImage`**.

A `.deb` targets Debian/Ubuntu and an `.rpm` targets Fedora/RHEL — an **Arch** student
would have to convert or extract them. The AppImage is the one file that works
everywhere: it bundles its own WebKit and 209 shared libraries, so it doesn't care what
distro you run.

Verify that claim for any build:

```bash
./scripts/verify-appimage.sh "path/to/App.AppImage"
```

It extracts the AppImage and asserts the libraries and WebKit are actually inside, so a
build that secretly depends on the host's libs fails loudly instead of breaking on a
student's machine.

## Local builds

```bash
./scripts/build-local.sh          # Linux: deb + rpm + AppImage, then verifies it
./scripts/build-local.sh          # macOS: dmg + app
```

Or the npm shortcuts:

```bash
npm run build:linux
npm run build:mac
npm run build:win     # must be run ON Windows (-setup.exe; the plain .exe is in target/release)
```

## Code signing

The builds use an **ad-hoc signature** (`"signingIdentity": "-"` in `tauri.conf.json`).
This is not optional on Apple Silicon: macOS **requires** a signature on any app
downloaded from the internet, and without one the `.dmg` reports the app *"is damaged and
can't be opened"* — which reads as malware to a student. Tauri's own docs name this exact
setting for that case.

Ad-hoc signing fixes the "damaged" hard failure. It does **not** make the app trusted, so
a browser-downloaded copy still hits Gatekeeper:

- **macOS** — *"Apple could not verify 'AI Builder School Setup'…"*. Since Sequoia 15 the
  old right-click → Open bypass is gone. The student has to click **Done**, then go to
  System Settings → Privacy & Security → Security → **Open Anyway** → password (Touch ID
  works). That is why the README leads with the Terminal one-liner, which skips the
  prompt. (`xattr -dr com.apple.quarantine "/Applications/AI Builder School Setup.app"`
  also works on a `.dmg` install.)
- **Windows** — SmartScreen shows "Windows protected your PC" → *More info* → *Run anyway*.

> If a student still sees "damaged", the signature is missing or broken, not merely
> untrusted.

Removing these prompts entirely needs a paid identity:

| Platform | Requirement | Cost |
|---|---|---|
| macOS | Apple Developer Program, then set `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` as repo secrets | $99/year |
| Windows | EV code-signing certificate | a few hundred/year |

The only zero-prompt, long-term fix on macOS is Apple Developer ID signing **plus
notarization** (~$99/year). The one-liner is a workaround, not a substitute. For a
classroom, ad-hoc signing plus the one-liner and clear instructions is usually acceptable;
revisit if students start bouncing off it.

## Release checklist

1. Bump `version` in `src-tauri/tauri.conf.json`, `package.json` **and** `src-tauri/Cargo.toml` (keep them equal).
2. Commit, then tag and push the tag.
3. Wait for all four runners to go green.
4. Publish the draft Release.
5. Download the AppImage yourself and run `scripts/verify-appimage.sh` on it.
6. Hand students **one** link — the Releases page. The release body already names the
   right file per OS (portable `.exe`, the macOS and Linux one-liners).