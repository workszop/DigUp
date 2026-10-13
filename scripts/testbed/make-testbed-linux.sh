#!/bin/bash
# The Linux smoke testbed: a few files of each kind with known content, for evals/linux-smoke.local.json.
# (make-testbed.sh needs macOS pictures, voices and AppKit; evals/code.json's repos come from make_code.py here too.)
#   scripts/testbed/make-testbed-linux.sh [dir]     default: ~/DigUpTestbed
# Needs ffmpeg, espeak-ng, google-chrome (or chromium) and python3.
set -euo pipefail
TB="${1:-$HOME/DigUpTestbed}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
CHROME="$(command -v google-chrome || command -v chromium || true)"
for tool in ffmpeg espeak-ng python3; do command -v "$tool" > /dev/null || { echo "needs $tool"; exit 1; }; done
[[ -n "$CHROME" ]] || { echo "needs google-chrome or chromium"; exit 1; }
[[ -e "$TB" ]] && { echo "$TB already exists; delete it first (rm -rf \"$TB\")"; exit 1; }
S="$TB/linux"; mkdir -p "$S/screenshots" "$S/docs" "$S/audio" "$S/video"

# Speech: one sentence per recording, 16 kHz mono like the indexer reads it.
speak() { espeak-ng -v en-us -s 150 -w "$S/audio/tmp.wav" "$2"; ffmpeg -loglevel error -y -i "$S/audio/tmp.wav" -ac 1 -ar 16000 "$S/audio/$1.wav"; rm "$S/audio/tmp.wav"; }
speak sleep_podcast "Tonight we talk about sleep. Most adults need seven to nine hours of sleep, and a dark, cool bedroom helps you fall asleep faster."
speak volcano_lecture "The volcano erupted at dawn. Lava flowed down the mountain and ash covered the nearby villages for days."

# Screenshots and PDFs: HTML with known words, rendered by headless Chrome. Screenshot names follow the prefix
# DigUp recognizes on Linux (Rules.screenshotPrefixes), since Linux has no screenshot marker.
page() {   # name, title, body
  printf '<!doctype html><meta charset="utf-8"><body style="font:20px sans-serif;margin:40px"><h1>%s</h1><p>%s</p>' "$2" "$3" > "$S/tmp-$1.html"
}
page payment "Payment declined" "Error 402: your card was declined. Invoice INV-20431 is still unpaid."
"$CHROME" --headless=new --no-sandbox --hide-scrollbars --window-size=1440,900 --screenshot="$S/screenshots/Screenshot 2026-10-13 at 09.12.44.png" "file://$S/tmp-payment.html" 2> /dev/null
page lease "Residential lease" "Clause 14. Pets: the tenant may keep one cat or one small dog with the landlord's written consent. A pet deposit of 300 euros applies."
"$CHROME" --headless=new --no-sandbox --print-to-pdf="$S/docs/lease agreement.pdf" --no-pdf-header-footer "file://$S/tmp-lease.html" 2> /dev/null
rm "$S"/tmp-*.html
printf '# Tokyo trip\n\nFlight to Tokyo Haneda on November 3. Then the train to Kamakura to see the Great Buddha.\n' > "$S/docs/tokyo.md"
printf 'Ilish maach recipe: ইলিশ মাছ সর্ষে বাটা দিয়ে রান্না করুন।\n' > "$S/docs/ইলিশ রান্না.txt"

# A video: the lecture's sound over a still frame (a real photo when one is given as $PHOTO, else a test pattern).
if [[ -n "${PHOTO:-}" ]]; then
  ffmpeg -loglevel error -loop 1 -i "$PHOTO" -i "$S/audio/volcano_lecture.wav" -vf "scale=1280:-2" -c:v libx264 -tune stillimage -c:a aac -shortest "$S/video/lecture.mp4"
else
  ffmpeg -loglevel error -f lavfi -i testsrc2=size=1280x720:rate=25 -i "$S/audio/volcano_lecture.wav" -c:v libx264 -c:a aac -shortest "$S/video/lecture.mp4"
fi

# Code: the repos evals/code.json expects.
python3 -I "$REPO/scripts/testbed/make_code.py" "$TB/code"
echo "Testbed in $TB"
