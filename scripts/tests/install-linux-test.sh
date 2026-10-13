#!/bin/bash
# install-linux.sh into a throwaway HOME (with a space in it, as some are): files in place, one shortcut however often
# it runs, the user's own shortcuts kept whatever shape their file is in, an update that leaves no old libraries
# behind, a failed library check that leaves the installed DigUp as it was, --uninstall leaving nothing of DigUp behind.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
ROOT="$(mktemp -d)"; trap 'rm -rf "$ROOT"' EXIT
export HOME="$ROOT/home dir"; mkdir -p "$HOME"
# A stand-in systemctl that only notes what it was asked, so a test can tell whether the service was touched.
export SYSTEMCTL_LOG="$ROOT/systemctl.log"
printf '#!/bin/sh\necho "$*" >> "$SYSTEMCTL_LOG"\n' > "$ROOT/systemctl"; chmod +x "$ROOT/systemctl"
export SYSTEMCTL="$ROOT/systemctl" DIGUP_SKIP_LDD=1 XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
SHORTCUTS="$HOME/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"
OPT="$HOME/.local/opt/digup"
fail() { echo "FAIL: $*"; exit 1; }
install_digup() { "$REPO/scripts/install-linux.sh" "$@" > /dev/null; }

# The shortcuts file is still a RON map COSMIC can read: braces around it, parentheses balanced, a comma between
# entries, the user's entry kept, and DigUp's there exactly once (or not at all).
valid_shortcuts() {   # what (for the message), how many DigUp entries
  python3 -I - "$SHORTCUTS" "$2" <<'PY' || fail "$1: shortcuts file broken: $(cat "$SHORTCUTS")"
import re, sys
path, digup = sys.argv[1], int(sys.argv[2])
text = open(path).read().strip()
assert text.startswith("{") and text.endswith("}"), "not a map"
assert text.count("(") == text.count(")"), "parentheses unbalanced"
assert not re.search(r"\)\s*\(", text), "two entries without a comma between them"
assert text.count("digup-shell --toggle") == digup, "DigUp entries: %d" % text.count("digup-shell --toggle")
PY
}

# MARK: A fresh install

# A COSMIC config folder, but no shortcuts file yet.
mkdir -p "$HOME/.config/cosmic"
install_digup
for f in .local/opt/digup/digup .local/opt/digup/digup-shell .local/opt/digup/ffmpeg .local/opt/digup/ffprobe .local/bin/digup-shell .local/share/applications/digup.desktop \
         .local/share/icons/hicolor/128x128/apps/digup.png .config/systemd/user/digup.service; do
  [[ -e "$HOME/$f" ]] || fail "missing $f"
done
valid_shortcuts "new file" 1
# Paths go into the menu entry and the unit quoted, so a HOME with a space in it still starts DigUp.
grep -qxF "Exec=\"$OPT/digup-shell\" --toggle" "$HOME/.local/share/applications/digup.desktop" || fail "menu entry Exec not quoted"
grep -qxF "ExecStart=\"$OPT/digup-shell\"" "$HOME/.config/systemd/user/digup.service" || fail "unit ExecStart not quoted"
if command -v desktop-file-validate > /dev/null; then
  desktop-file-validate "$HOME/.local/share/applications/digup.desktop" || fail "menu entry doesn't validate"
fi

# MARK: The user's shortcuts

# The user's own shortcuts, then two installs.
cat > "$SHORTCUTS" <<'RON'
{
    (
        modifiers: [
            Super,
        ],
        key: "v",
        description: Some("clip"),
    ): Spawn("/home/x/bin/clippy.sh pick"),
}
RON
install_digup
install_digup
valid_shortcuts "two installs" 1
grep -q 'clippy.sh pick' "$SHORTCUTS" || fail "user's shortcut lost"

# MARK: Uninstall

install_digup
cat > "$SHORTCUTS" <<'RON'
{
    (
        modifiers: [
            Super,
        ],
        key: "v",
        description: Some("clip"),
    ): Spawn("/home/x/bin/clippy.sh pick"),
}
RON
install_digup
install_digup --uninstall
[[ ! -e "$OPT" && ! -e "$HOME/.local/bin/digup-shell" && ! -e "$HOME/.config/systemd/user/digup.service" ]] \
  || fail "uninstall left files"
valid_shortcuts "uninstall" 0
grep -q 'clippy.sh pick' "$SHORTCUTS" || fail "uninstall removed the user's shortcut"

# MARK: The library check

# A library the programs need but this machine lacks stops the install, naming it, before the service is enabled.
# A stand-in ldd makes that deterministic, whatever this host has installed.
mkdir -p "$ROOT/ldd-missing"
printf '#!/bin/sh\necho "\tlibfake.so.9 => not found"\n' > "$ROOT/ldd-missing/ldd"; chmod +x "$ROOT/ldd-missing/ldd"
if out="$(env -u DIGUP_SKIP_LDD PATH="$ROOT/ldd-missing:$PATH" "$REPO/scripts/install-linux.sh" 2>&1)"; then
  fail "install succeeded with a library missing"
fi
grep -q 'libfake.so.9' <<< "$out" || fail "missing library not named: $out"
echo "install-linux: ok"
