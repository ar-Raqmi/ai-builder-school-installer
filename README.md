<div align="center">
  <img src="assets/brand/icon-mono-1024.png" alt="AI Builder School" width="120">
  <h1>AI Builder School Installer</h1>
  <p><em>Press install, paste your API key, start vibe coding.</em></p>
</div>

I've been enjoying entering the **AI Builder School**, but the problem I kept seeing is that we always waste so much time on the setup session instead of the actual coding. Every class someone's stuck on installing VSCode, or Git isn't on the PATH, or Kilo Code isn't connected or the API key isn't working and by the time everyone's sorted, half the session is gone. :<

So I made this **AI Builder School Installer** :>

## Download

Grab the file for your machine from the [**Releases page**](../../releases/latest):

| Your computer | Download |
|---|---|
| **Windows** | `aibs-setup_windows_x64.exe` (portable, just double-click) |
| **macOS** | the Terminal one-liner below (or the `.dmg`) |
| **Linux** | the Terminal one-liner below (or the `.AppImage`) — works on Arch, Ubuntu, Fedora, anything |

### Windows

Double-click `aibs-setup_windows_x64.exe`. There's nothing to install first, it just runs. Windows might say "Windows protected your PC" because I don't have a paid code-signing certificate — **More info** → **Run anyway**.

It needs the WebView2 runtime, which Windows 11 and current Windows 10 already have. If the portable exe doesn't open, use the `-setup.exe` instead — it installs WebView2.

### macOS

The app isn't notarized (that needs Apple's paid $99/year developer program), so macOS doesn't know who I am and has opinions about that. Two ways around it:

**Option A (recommended, no security prompts).** Open Terminal and paste:

```bash
curl -fsSL https://raw.githubusercontent.com/ar-Raqmi/ai-builder-school-installer/master/scripts/install-mac.sh | bash
```

It installs **Homebrew** if you don't have it yet (the wizard needs it for VS Code and Node, and can't install it itself — it has no terminal to ask for your password with), then installs the app into `/Applications` (or `~/Applications` if that's locked) and opens it. Expect it to ask for your Mac's password once, and to take a while if the Xcode Command Line Tools have to be installed first.

macOS only gets suspicious of files that came through a browser, and `curl` isn't one, so it never asks.

**Option B (the `.dmg`).** Drag the app to Applications and double-click it. It says *"Apple could not verify 'AI Builder School Setup'…"* — click **Done**, then:

1. Open **System Settings**
2. Go to **Privacy & Security**
3. Scroll down to **Security**
4. Under "Allow applications from" select **App Store & Known Developers**
5. Next to *"'AI Builder School Setup' was blocked to protect your Mac."* click **Open Anyway**
6. Enter your password (Touch ID works too) → **Open**

Only needed once. The old right-click → Open trick stopped working in macOS Sequoia 15, so don't bother with it.

### Linux

Open a terminal and paste:

```bash
curl -fsSL https://raw.githubusercontent.com/ar-Raqmi/ai-builder-school-installer/master/scripts/install-linux.sh | bash
```

It finds the latest AppImage for your computer, saves it as `~/.local/bin/ai-builder-school-setup` and opens it. Any time after that you can open it again with `ai-builder-school-setup`. It never uses `sudo`. AppImages normally need FUSE (`libfuse2`); if you don't have it the script notices and unpacks the app first, so it still works.

**Prefer to download it yourself?** Grab the `.AppImage` from the Releases page, not the `.deb` or `.rpm`. The AppImage is self-contained so it runs on any distro; the `.deb` is only for Debian/Ubuntu and the `.rpm` only for Fedora. Then make it executable (`chmod +x` on the file) and run it.

![System check](docs/screenshots/01-detect.png)

It handles the whole boring part in one go. You double-click it, and it sets up **VS Code → Git → Node.js → Kilo Code → your API key**. Then you just open VS Code, open your folder, click Kilo Code and you're building.

---

## What it actually does

**It checks first, then installs.** If you already have VSCode, it skips it and leaves it alone. Same for Git and Node. Nothing gets reinstalled or overwritten, so if your machine is already set up for other stuff, it stays that way.

**Then it installs whatever's missing.** Windows uses `winget`, macOS uses `brew`, Linux figures out whether you're on Arch, Debian or Fedora and uses the right one. You don't have to know any of this — it just does it.

**Then Kilo Code gets installed into VSCode** so the extension is already there when you open the editor.

**Then you paste your API key.** Hit *Test connection* and it pings the gateway for real — if the key is wrong you find out immediately, not halfway through a lesson. Once it connects, it pulls the live list of models so you can pick which one you want.

![Connect your API key](docs/screenshots/03-models.png)

**Don't have a key yet?** There's a *Skip* button. It finishes the setup anyway and you can use Kilo Code's own free models in the meantime. Nothing breaks.

**Then it's done,** and it tells you exactly what to do next.

![Setup complete](docs/screenshots/04-done.png)

---

## Your API key

From your **KrackedDevs profile dashboard** — the installer links straight to it so you don't have to go hunting:
<https://krackeddevs.com/dashboard/profile>

The gateway is `https://litellm.rapidscreen.io/v1`, OpenAI-compatible, so it works as a custom provider in Kilo Code.

## Which model?

The default is **`deepseek-v4.1-flash`** — it's fast and cheap and more than enough for the course. You can pick something else during setup and it remembers your choice next time. The list comes from the gateway live, so whatever's actually available right now is what you'll see.

There's also `gpt-5.6-sol`, `gpt-6-astra`, `no-claude-models-bruh` and plenty more if you want to poke around.

---

## Dark mode

Because of course.

![Dark theme](docs/screenshots/05-dark.png)

---

## If you're running it yourself

Only three things to know: **Rust**, **Node 18+**, and on Linux also `webkit2gtk-4.1` and `base-devel`.

```bash
npm install
npm run dev
```

To build the installers:

```bash
npm run build:linux    # .deb, .AppImage, .rpm
npm run build:mac      # .dmg, .app
npm run build:win      # -setup.exe (the plain .exe is in target/release)
```

macOS and Windows bundles have to be built **on** macOS and Windows — that's a Tauri/WebKit thing, not something I can work around. There's a GitHub Actions workflow that builds all three on native runners.

There's a test suite if you're poking at the code — `npm run test:ui` drives the whole flow in a real browser, and `cargo test` runs the Rust units. The live tests hit the real gateway, so they're opt-in:

```bash
cd src-tauri
AIBS_TEST_KEY=sk-... cargo test --test live -- --ignored
```

---

## Status

What I've actually run, so you know what to trust:

- **Linux** (Arch / EndeavourOS): tested. The app builds, opens and runs its system check.
- **macOS** (Apple silicon): tested the same way.
- **Windows**: tested on a real Windows VM and it works. The portable exe launches, no console windows pop up, and detection and installs go through.

What I *haven't* tested, so treat these as unverified:

- **A clean macOS machine** (no Homebrew) running the **`.dmg`** path. The wizard alone
  cannot install Homebrew: it runs the installer without a terminal, so the admin prompt
  can't be answered, and VS Code and Node then fall back to manual instructions. The
  Terminal one-liner installs Homebrew for you before the app opens — that combination is
  the one to use on a fresh Mac.
- The **macOS Intel** build on real Intel hardware. It's built by CI, but I haven't run it.
- The **`.deb` and `.rpm`** packages. I've only run the AppImage.
- **`install-linux.sh`**. It has only been exercised on a Mac against a faked Linux (stubbed `uname` and `curl`, a pretend AppImage), which covers the architecture check, picking the right file, the no-FUSE fallback and the error messages. It hasn't run on a real Linux machine against a real release yet, and the Linux builds are x86_64 only for now.

If something breaks, tell me.

---

<div align="center">

  *🖋️ the pen hasn't lifted*

  setup by <a href="https://ar-raqmi.net/">ar-Raqmi</a>

</div>