#!/bin/bash
# Installs the DigUp you built (swift build -c release --static-swift-stdlib, cargo build --release in shell/src-tauri)
# for this user on Linux: the programs and their libraries in ~/.local/opt/digup, a menu entry, a systemd user service
# that starts it with the desktop, and on COSMIC a Ctrl+Shift+Space shortcut to `digup-shell --toggle` (Wayland gives
# no app a global hotkey). Nothing needs root.
#   scripts/install-linux.sh                 install, or update in place
#   scripts/install-linux.sh --uninstall     remove all of it (the index and the model stay)
#   scripts/install-linux.sh --purge         remove those too
# Env overrides, for tests: SYSTEMCTL (default systemctl) replaces the systemctl command; DIGUP_SKIP_LDD=1 skips the
# check that every library the programs need is installed on this machine.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SYSTEMCTL="${SYSTEMCTL:-systemctl}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
OPT="$HOME/.local/opt/digup"
BIN="$HOME/.local/bin"
UNIT="$CONFIG/systemd/user/digup.service"
DESKTOP="$DATA/applications/digup.desktop"
ICON="$DATA/icons/hicolor/128x128/apps/digup.png"
SHORTCUTS="$CONFIG/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"

# COSMIC's custom shortcuts are a RON map; DigUp's entry is added or removed whole, the rest kept as written.
shortcut() {   # add | remove
  [[ "$1" == remove && ! -f "$SHORTCUTS" ]] && return 0
  mkdir -p "$(dirname "$SHORTCUTS")"
  python3 -I - "$1" "$SHORTCUTS" "$BIN/digup-shell --toggle" <<'PY'
import os, re, sys
action, path, command = sys.argv[1:]
text = open(path).read() if os.path.exists(path) else "{\n}\n"
entry = re.compile(r'\n?[ \t]*\(\s*modifiers:[^)]*\],\s*key:[^)]*?description: Some\("digup"\),\s*\): Spawn\("[^"]*"\),')
text = entry.sub("", text)
if action == "add":
    block = ('\n    (\n        modifiers: [\n            Ctrl,\n            Shift,\n        ],\n        key: "space",\n'
             '        description: Some("digup"),\n    ): Spawn("%s"),' % command)
    end = text.rstrip().rfind("}")
    text = text[:end].rstrip() + block + "\n}\n"
tmp = path + ".digup-new"
open(tmp, "w").write(text)
os.replace(tmp, path)
PY
}

uninstall() {
  "$SYSTEMCTL" --user disable --now digup.service 2> /dev/null || true
  rm -rf "$OPT" "$BIN/digup" "$BIN/digup-shell" "$UNIT" "$DESKTOP" "$ICON"
  shortcut remove
  "$SYSTEMCTL" --user daemon-reload 2> /dev/null || true
}

case "${1:-}" in
  --uninstall) uninstall; echo "DigUp removed (index and model kept in $DATA/digup)"; exit 0 ;;
  --purge) uninstall; rm -rf "$DATA/digup" "$CONFIG/com.abdurrahim.DigUp"; echo "DigUp removed, with its index and model"; exit 0 ;;
  "") ;;
  *) echo "usage: $0 [--uninstall | --purge]"; exit 2 ;;
esac

HELPER="$REPO/.build/release/digup"
SHELL_BIN="$REPO/shell/src-tauri/target/release/digup-shell"
FFMPEG_DIR="$REPO/Vendor/ffmpeg/bin"
for built in "$HELPER" "$SHELL_BIN"; do
  [[ -x "$built" ]] || { echo "build it first: $built is missing"; exit 1; }
done
for tool in ffmpeg ffprobe; do
  [[ -x "$FFMPEG_DIR/$tool" ]] || { echo "build it first: $FFMPEG_DIR/$tool is missing (scripts/fetch-vendor.sh fetches it)"; exit 1; }
done

# The new DigUp is put together beside the old one and checked there; only then does it replace the old one, whole.
# An update leaves nothing of the last build behind (a llama.cpp backend it no longer has would still be loaded), and a
# failed check leaves the installed DigUp, and the running service, as they were.
STAGE="$OPT.new"
rm -rf "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE" "$BIN" "$(dirname "$UNIT")" "$(dirname "$DESKTOP")" "$(dirname "$ICON")"
# Beside the programs: digup finds its libraries there ($ORIGIN rpath) and digup-shell finds digup there.
install -m 755 "$HELPER" "$SHELL_BIN" "$STAGE/"
# The engine needs ffmpeg 8.1 or newer, so it takes its own, which it looks for beside itself first.
install -m 755 "$FFMPEG_DIR/ffmpeg" "$FFMPEG_DIR/ffprobe" "$STAGE/"
cp -a "$REPO"/Vendor/llama/lib/*.so* "$REPO"/Vendor/pdfium/lib/*.so "$STAGE/"
# A library this machine lacks would only show up as a service that never starts, so say so now, before enabling it.
if [[ "${DIGUP_SKIP_LDD:-}" != 1 ]]; then
  missing=""
  for program in digup digup-shell; do
    if ! linked="$(ldd "$STAGE/$program" 2>&1)"; then
      echo "DigUp couldn't check the libraries $program needs (ldd failed):"
      sed 's/^/  /' <<< "$linked"
      exit 1
    fi
    missing+="$(awk '/not found/ { print $1 }' <<< "$linked")"$'\n'
  done
  missing="$(sort -u <<< "$missing" | sed '/^$/d')"
  if [[ -n "$missing" ]]; then
    echo "DigUp needs libraries this machine does not have:"
    sed 's/^/  /' <<< "$missing"
    echo "Install the package that provides each one (for example: apt search <name>), then run this again."
    exit 1
  fi
fi
"$SYSTEMCTL" --user stop digup.service 2> /dev/null || true
rm -rf "$OPT"
mv "$STAGE" "$OPT"
ln -sfn "$OPT/digup" "$BIN/digup"
ln -sfn "$OPT/digup-shell" "$BIN/digup-shell"
install -m 644 "$REPO/shell/src-tauri/icons/128x128.png" "$ICON"

# Each format quotes a path its own way, so a HOME with a space (or a quote, or a $) in it still starts DigUp.
# A menu entry's Exec: double quotes, with \ " ` $ escaped inside them, then every \ doubled again (the value is a
# string first) and % doubled (field codes).
desktop_quote() { printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/\\/\\\\/g' -e 's/%/%%/g')"; }
# The program in a unit's ExecStart: double quotes, with \ and " escaped inside them and % doubled (specifiers). A $
# stays as it is: systemd expands no variables in the program itself, so "$$" there would be two dollars.
systemd_quote() { printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"]/\\&/g' -e 's/%/%%/g')"; }

cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=DigUp
Comment=Search your files by what's in them
Exec=$(desktop_quote "$OPT/digup-shell") --toggle
Icon=digup
Terminal=false
Categories=Utility;
StartupNotify=false
EOF

# KillMode=process: what DigUp opened (a video player, an editor) stays open when the service restarts; DigUp's own
# helpers quit with it anyway (they watch their parent).
cat > "$UNIT" <<EOF
[Unit]
Description=DigUp: search your files by what's in them
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=$(systemd_quote "$OPT/digup-shell")
Restart=on-failure
KillMode=process

[Install]
WantedBy=graphical-session.target
EOF

if [[ "${XDG_CURRENT_DESKTOP:-}" == *COSMIC* || -d "$CONFIG/cosmic" ]]; then shortcut add; fi
"$SYSTEMCTL" --user daemon-reload
"$SYSTEMCTL" --user enable --now digup.service
echo "DigUp installed in $OPT. Search with Ctrl+Shift+Space (COSMIC), or run: digup-shell --toggle"
