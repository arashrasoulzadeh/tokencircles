#!/bin/bash
# Render TokenHUD views to transparent PNGs using the real dist/ styles + JS.
set -e
REPO=~/Documents/projects/tokenhud
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT="$REPO/docs"
TMP=$(mktemp -d)
mkdir -p "$OUT"

STYLE=$(awk '/<style>/,/<\/style>/' "$REPO/dist/index.html")
SCRIPT_BODY=$(awk '/<script>/{f=1;next} /<\/script>/{f=0} f' "$REPO/dist/index.html" \
  | sed '/async function fitCard/,$d')   # keep helpers, drop fitCard + Tauri wiring
SCRIPT_BODY="$SCRIPT_BODY
  function fitCard(){}"                  # stub so nothing references a missing fn

make_page () {  # $1 out-html  $2 body-class  $3 js-render-call  $4 width  $5 height
  cat > "$1" <<EOF
<!doctype html><html><head><meta charset="utf-8">
$STYLE
<style>
  html,body{background:transparent!important}
  body{width:${4}px;height:${5}px}
  #foot{visibility:hidden}            /* timestamp is non-deterministic */
</style></head>
<body class="$2">
  <div class="card" id="card">
    <header>
      <span>Token<b>HUD</b></span>
      <span>
        <button id="side">&#8646;</button><button id="mode">&#9679;</button>
        <button id="settings">&#9881;</button><button id="close">&times;</button>
      </span>
    </header>
    <div id="body"></div>
    <footer id="foot">updated</footer>
  </div>
  <div id="rings"></div>
<script>
$SCRIPT_BODY
$3
</script></body></html>
EOF
}

shot () {  # $1 page-html  $2 out-png  $3 w  $4 h
  "$CHROME" --headless --disable-gpu --hide-scrollbars \
    --force-device-scale-factor=2 \
    --default-background-color=00000000 \
    --window-size=$3,$4 \
    --screenshot="$TMP/raw.png" "file://$1" >/dev/null 2>&1
  python3 - "$TMP/raw.png" "$2" <<'PY'
import sys
from PIL import Image
src, dst = sys.argv[1], sys.argv[2]
im = Image.open(src).convert("RGBA")
alpha = im.split()[3].point(lambda p: 255 if p > 24 else 0)
bb = alpha.getbbox()
im = im.crop(bb)
pad = 16
out = Image.new("RGBA", (im.width + pad*2, im.height + pad*2), (0, 0, 0, 0))
out.paste(im, (pad, pad), im)
out.save(dst)
print(f"wrote {dst}  {out.size}")
PY
}

# --- sample data -------------------------------------------------------------
CLAUDE='{tool:"claude",hour:{total:9_100_000,cost_usd:2.4,cap:null,ratio:null},
  five_h:{total:41_000_000,cost_usd:11.0,cap:null,ratio:null},
  week:{total:527_000_000,cost_usd:130.0,cap:null,ratio:null},
  week_by_model:[["claude-sonnet-5",527000000]],
  rate_limits:[{window_label:"5h",used_percent:18,observed_at:Date.now()/1000},
               {window_label:"weekly",used_percent:17,observed_at:Date.now()/1000}],
  advisories:[]}'
CODEX='{tool:"codex",hour:{total:0,cost_usd:0,cap:null,ratio:null},
  five_h:{total:0,cost_usd:0,cap:null,ratio:null},
  week:{total:0,cost_usd:0,cap:null,ratio:null},week_by_model:[],
  rate_limits:[{window_label:"weekly",used_percent:96,observed_at:Date.now()/1000}],
  advisories:[{severity:"critical",text:"weekly 96% used, resets Wed 17:27"}]}'

# 1. circle mode — the default
make_page "$TMP/circle.html" circle "renderCircle([$CLAUDE])" 90 180
shot "$TMP/circle.html" "$OUT/circle.png" 200 380

# 2. card mode — single tool, authoritative %
make_page "$TMP/card.html" card "document.getElementById('body').appendChild(toolBlock($CLAUDE));" 280 200
shot "$TMP/card.html" "$OUT/card.png" 620 460

# 3. card mode — two tools + an advisory
make_page "$TMP/card2.html" card "
  const b=document.getElementById('body');
  b.appendChild(toolBlock($CLAUDE));
  b.appendChild(el('div','sep'));
  b.appendChild(toolBlock($CODEX));" 280 260
shot "$TMP/card2.html" "$OUT/card-multi.png" 620 600

rm -rf "$TMP"
