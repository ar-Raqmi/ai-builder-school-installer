#!/usr/bin/env bash
# Install AI Builder School Setup on macOS without Gatekeeper's "Apple could not verify" prompt.
#
# Why this works: Gatekeeper only assesses files carrying the com.apple.quarantine attribute.
# Browsers add it to downloads; curl does not, and tar does not copy it out of an archive.
# The app is ad-hoc signed, so it runs fine on Apple silicon - the prompt is the only problem.
#
# It also installs Homebrew when it's missing, because the wizard needs it for VS Code and
# Node and cannot install it alone: a GUI app has no terminal, so Homebrew's admin prompt
# can never be answered. Here the password prompt is a real one.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ar-Raqmi/ai-builder-school-installer/master/scripts/install-mac.sh | bash

set -euo pipefail

RELEASES="https://github.com/ar-Raqmi/ai-builder-school-installer/releases"
APP="AI Builder School Setup.app"
BREW_INSTALLER="https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh"

# Homebrew is how the wizard installs VS Code and Node, and it cannot install Homebrew
# itself: the app has no terminal, so the installer's admin prompt can never be answered.
# Doing it here — where there IS a terminal — is what lets a fresh Mac finish setup.
find_brew() {
  local candidate
  for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [[ -x "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done
  command -v brew 2> /dev/null
}

ensure_brew() {
  local brew
  brew="$(find_brew || true)"
  if [[ -n "$brew" ]]; then
    echo "Homebrew found ($brew)."
    return 0
  fi

  if ! (: < /dev/tty) 2> /dev/null; then
    echo "Homebrew isn't installed, and there's no terminal here to ask for your password."
    return 1
  fi

  echo
  echo "Homebrew isn't installed yet. The wizard needs it to install VS Code and Node."
  echo "The installer asks for your Mac password, and may install the Xcode Command Line"
  echo "Tools first (that part can take ten minutes or so)."
  echo

  local installer
  installer="$(mktemp)"
  if ! curl -fsSL "$BREW_INSTALLER" -o "$installer"; then
    rm -f "$installer"
    echo "Couldn't download the Homebrew installer. Check your internet connection."
    return 1
  fi
  # stdin has to be the terminal. Piped in, this script's own stdin is the curl pipe, and
  # Homebrew's installer treats a non-TTY stdin as non-interactive — which turns its sudo
  # calls into `sudo -n` and fails on the password prompt.
  if ! /bin/bash "$installer" < /dev/tty; then
    rm -f "$installer"
    echo "The Homebrew installer didn't finish."
    return 1
  fi
  rm -f "$installer"

  brew="$(find_brew || true)"
  if [[ -z "$brew" ]]; then
    echo "The Homebrew installer finished but brew isn't there — please tell ar-Raqmi."
    return 1
  fi
  echo "Homebrew installed ($brew)."
}

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script is for macOS only."
  echo "For Windows or Linux, download the right file from $RELEASES/latest"
  exit 1
fi

case "$(uname -m)" in
  arm64) arch="aarch64" ;;
  x86_64) arch="x64" ;;
  *)
    echo "Sorry, I don't recognise this Mac's processor ($(uname -m))."
    echo "You can still download the .dmg from $RELEASES/latest"
    exit 1
    ;;
esac

tarball="AI.Builder.School.Setup_${arch}.app.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading AI Builder School Setup ($arch)..."
if ! curl -fL "$RELEASES/latest/download/$tarball" -o "$tmp/$tarball"; then
  echo "Couldn't download it. Check your internet connection, or get the .dmg from $RELEASES/latest"
  exit 1
fi

# After the download, so a Homebrew failure never costs the student the app itself.
if ! ensure_brew; then
  echo
  echo "Continuing without Homebrew: the wizard will still install, and will ask you to"
  echo "install Homebrew yourself before it can set up VS Code and Node."
fi

tar -xzf "$tmp/$tarball" -C "$tmp"
if [[ ! -d "$tmp/$APP" ]]; then
  echo "The download didn't contain \"$APP\" - please tell ar-Raqmi."
  exit 1
fi

# Never sudo: if /Applications isn't writable, a per-user folder works just as well.
if [[ -w /Applications ]]; then
  dest_dir="/Applications"
else
  dest_dir="$HOME/Applications"
  mkdir -p "$dest_dir"
fi
dest="$dest_dir/$APP"

rm -rf "${dest:?}"
mv "$tmp/$APP" "$dest"
# Insurance only: nothing above should have set the attribute.
xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true

echo "Installed to $dest"
open "$dest"
