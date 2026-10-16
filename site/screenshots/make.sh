#!/usr/bin/env bash
# Take the handbook's screenshots from the real apps (needs a display) and
# write optimised PNGs to site/static/screenshots/.
#   cargo build --workspace && site/screenshots/make.sh
# Under XWayland set WGPU_BACKEND=vulkan. Nothing here touches your own
# settings: the viewer and server get throw-away config folders.
set -euo pipefail
cd "$(dirname "$0")/../.."
BIN=target/debug
OUT=site/static/screenshots
T=$(mktemp -d)
trap 'rm -rf "$T"; kill $(jobs -p) 2>/dev/null || true' EXIT
mkdir -p "$OUT"
rm -f "$OUT"/*.png
now=$(date +%s)

viewer_config() { # THEME
  mkdir -p "$T/viewer/farpane"
  cat > "$T/viewer/farpane/viewer.toml" <<TOML
version = 1
[appearance]
theme = "$1"
[[connection]]
name = "build-01"
addr = "build-01.lab.example:5900"
last_connected = $((now - 600))
[[connection]]
name = "Lab kiosk"
addr = "kiosk.lab.example:5900"
last_connected = $((now - 86400))
[[connection]]
name = "Windows test VM"
addr = "10.0.4.31:5901"
last_connected = $((now - 259200))
[[known_host]]
host = "build-01.lab.example:5900"
sha256 = "3F:9A:4C:11:7B:02:E8:5D:90:A6:3C:F1:0D:B4:2E:77:19:C8:6A:50:E3:94:AB:1F:D2:68:35:7C:0E:91:FA:C4"
TOML
}
viewer() { # NAME THEME [ARGS...]
  local name=$1 theme=$2; shift 2
  viewer_config "$theme"
  XDG_CONFIG_HOME="$T/viewer" timeout 60 $BIN/farpane-viewer --ui-screenshot "$T/$name.png" "$@" >/dev/null 2>&1 || true
}
server() { # NAME THEME PAGE CONFIGDIR PORT [CLIENTS]
  local name=$1 theme=$2 page=$3 cfg=$4 port=$5 clients=${6:-0}
  if [ "$clients" -gt 0 ]; then
    (sleep 2; python3 site/screenshots/hold.py "$port" 14 "$clients") &
  fi
  timeout 60 $BIN/farpane-server --config-dir "$cfg" --listen "127.0.0.1:$port" \
    ${EXTRA:-} --theme "$theme" --page "$page" --ui-delay 7 --ui-screenshot "$T/$name.png" >/dev/null 2>&1 || true
}
shrink() { # NAME
  python3 - "$T/$1.png" "$OUT/$1.png" <<'PY'
import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("RGB")
im.save(sys.argv[2], optimize=True)
print(sys.argv[2], im.size)
PY
}

# Viewer: home in three themes, then a live session with the stats card.
viewer viewer-home-glass glass
viewer viewer-home-slate slate
viewer viewer-home-light light
$BIN/farpane-server --headless --config-dir "$T/srv0" --listen 127.0.0.1:5951 >/dev/null 2>&1 &
sleep 1
viewer viewer-session glass --stats 127.0.0.1:5951
kill %1 2>/dev/null || true

# Server: no users, so plain test clients can connect.
mkdir -p "$T/srv1"
server server-overview glass overview "$T/srv1" 5952 3
server server-clients slate clients "$T/srv1" 5953 3
server server-security light security "$T/srv1" 5954

# Server: accounts for the Users page.
mkdir -p "$T/srv2"
for u in alice:admin bob:control ci-bot:view; do
  FARPANE_PASSWORD="correct horse battery" $BIN/farpane-server user add "${u%%:*}" --role "${u##*:}" \
    --config-dir "$T/srv2" >/dev/null 2>&1 || true
done
# Accounts need encryption, so this one runs with a self-signed certificate.
EXTRA="--tls-self-signed" server server-users glass users "$T/srv2" 5955

for n in viewer-home-glass viewer-home-slate viewer-home-light viewer-session \
         server-overview server-clients server-security server-users; do
  [ -f "$T/$n.png" ] && shrink "$n" || echo "missing $n" >&2
done
