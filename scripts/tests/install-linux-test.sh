#!/bin/bash
# install-linux.sh into a throwaway HOME: files in place, one shortcut however often it runs, the user's own shortcuts
# kept, --uninstall leaving nothing of DigUp behind.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
export HOME; HOME="$(mktemp -d)"; trap 'rm -rf "$HOME"' EXIT
export SYSTEMCTL=true DIGUP_SKIP_LDD=1 XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
SHORTCUTS="$HOME/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"
fail() { echo "FAIL: $*"; exit 1; }

# A COSMIC config folder, but no shortcuts file yet.
mkdir -p "$HOME/.config/cosmic"
"$REPO/scripts/install-linux.sh" > /dev/null
for f in .local/opt/digup/digup .local/opt/digup/digup-shell .local/opt/digup/ffmpeg .local/opt/digup/ffprobe .local/bin/digup-shell .local/share/applications/digup.desktop \
         .local/share/icons/hicolor/128x128/apps/digup.png .config/systemd/user/digup.service; do
  [[ -e "$HOME/$f" ]] || fail "missing $f"
done
[[ "$(grep -c 'digup-shell --toggle' "$SHORTCUTS")" == 1 ]] || fail "no single shortcut in a new file"

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
"$REPO/scripts/install-linux.sh" > /dev/null
"$REPO/scripts/install-linux.sh" > /dev/null
[[ "$(grep -c 'digup-shell --toggle' "$SHORTCUTS")" == 1 ]] || fail "shortcut duplicated"
grep -q 'clippy.sh pick' "$SHORTCUTS" || fail "user's shortcut lost"
python3 -I -c 'import sys; s=open(sys.argv[1]).read(); assert s.count("(") == s.count(")") and s.strip().startswith("{") and s.strip().endswith("}")' "$SHORTCUTS" \
  || fail "shortcuts file no longer balanced"

"$REPO/scripts/install-linux.sh" --uninstall > /dev/null
[[ ! -e "$HOME/.local/opt/digup" && ! -e "$HOME/.local/bin/digup-shell" && ! -e "$HOME/.config/systemd/user/digup.service" ]] \
  || fail "uninstall left files"
grep -q 'digup-shell' "$SHORTCUTS" && fail "uninstall left the shortcut"
grep -q 'clippy.sh pick' "$SHORTCUTS" || fail "uninstall removed the user's shortcut"
# A library the programs need but this machine lacks stops the install, naming it, before the service is enabled.
# A stand-in ldd makes that deterministic, whatever this host has installed.
mkdir -p "$HOME/fakebin"
printf '#!/bin/sh\necho "\tlibfake.so.9 => not found"\n' > "$HOME/fakebin/ldd"; chmod +x "$HOME/fakebin/ldd"
if out="$(env -u DIGUP_SKIP_LDD PATH="$HOME/fakebin:$PATH" SYSTEMCTL=false "$REPO/scripts/install-linux.sh" 2>&1)"; then
  fail "install succeeded with a library missing"
fi
grep -q 'libfake.so.9' <<< "$out" || fail "missing library not named: $out"
echo "install-linux: ok"
