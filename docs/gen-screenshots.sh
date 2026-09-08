#!/bin/bash
# Render TokenHUD's real dist/ UI (styles + hud.mjs) headless and assert the
# rendered DOM. With no args it also refreshes docs/*.png (needs Python/PIL);
# pass --check to run the assertions only (CI).
#
#   CHROME=/path/to/chrome bash docs/gen-screenshots.sh [--check]
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1
OUT="$REPO/docs"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$OUT"

cp "$REPO/dist/hud.mjs" "$TMP/hud.mjs"
STYLE="$(awk '/<style>/,/<\/style>/' "$REPO/dist/index.html")"

# --- deterministic sample data ---------------------------------------------
NOW=1788800000000            # fixed "now" so screenshots are reproducible
read -r -d '' DATA <<'JS' || true
const NOW = 1788800000000;
const CLAUDE = {
  tool: "claude",
  hour: { total: 9_100_000, cost_usd: 2.4, cap: null, ratio: null },
  five_h: { total: 41_000_000, cost_usd: 11.0, cap: null, ratio: null },
  week: { total: 527_000_000, cost_usd: 130.0, cap: null, ratio: null },
  week_by_model: [["claude-sonnet-5", 527000000]],
  rate_limits: [
    { window_label: "5h", used_percent: 18, observed_at: NOW / 1000 - 120 },
    { window_label: "weekly", used_percent: 17, observed_at: NOW / 1000 - 120 },
  ],
  five_h_reset: NOW / 1000 + 184 * 60,
  advisories: [],
};
const CODEX = {
  tool: "codex",
  hour: { total: 0, cost_usd: 0, cap: null, ratio: null },
  five_h: { total: 0, cost_usd: 0, cap: null, ratio: null },
  week: { total: 0, cost_usd: 0, cap: null, ratio: null },
  week_by_model: [],
  rate_limits: [{ window_label: "weekly", used_percent: 96, observed_at: NOW / 1000 - 300 }],
  advisories: [{ severity: "critical", text: "weekly 96% used, resets Wed 17:27" }],
};
JS

# $1 out.html  $2 body-class  $3 render-call
harness () {
  local w=284
  [ "$2" = circle ] && w=96
  cat > "$1" <<EOF
<!doctype html><html><head><meta charset="utf-8">
$STYLE
<style>
  html,body{background:transparent!important}
  html{width:${w}px;overflow:hidden}
  body{width:${w}px;overflow:hidden}
  #foot{visibility:hidden}
  body.card .card{width:268px}
  body.circle #rings{width:76px}
  .ctl{opacity:1}
</style></head>
<body class="$2">
  <div class="card" id="card">
    <header><span class="brand"><span class="mark"></span><b>TokenHUD</b></span>
    <span class="ctl"><button>&#8646;</button><button>&#9673;</button><button>&#9881;</button><button>&times;</button></span></header>
    <div id="body"></div><footer id="foot"></footer>
  </div>
  <div id="rings"></div>
<script type="module">
  import { renderCard, renderCircle } from "./hud.mjs";
  $DATA
  $3
  document.documentElement.dataset.ready = "1";
</script></body></html>
EOF
}

shot () {  # $1 html  $2 out.png  $3 w  $4 h
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars \
    --allow-file-access-from-files \
    --force-device-scale-factor=2 --default-background-color=00000000 \
    --window-size="$3,$4" --virtual-time-budget=1500 \
    --screenshot="$TMP/raw.png" "file://$1" >/dev/null 2>&1
  python3 - "$TMP/raw.png" "$2" <<'PY'
import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("RGBA")
alpha = im.split()[3].point(lambda p: 255 if p > 10 else 0)
im = im.crop(alpha.getbbox())
pad = 16
out = Image.new("RGBA", (im.width + pad * 2, im.height + pad * 2), (0, 0, 0, 0))
out.paste(im, (pad, pad), im)
out.save(sys.argv[2])
print(f"  wrote {sys.argv[2].split('/')[-1]}  {out.size[0]}x{out.size[1]}")
PY
}

check () {  # $1 html  $2 grep-pattern  $3 description
  local dom
  dom="$("$CHROME" --headless=new --disable-gpu --allow-file-access-from-files \
    --virtual-time-budget=1500 --dump-dom "file://$1" 2>/dev/null)"
  if grep -q 'data-ready="1"' <<<"$dom" && grep -qE "$2" <<<"$dom"; then
    echo "  ok  $3"
  else
    echo "  FAIL  $3" >&2
    exit 1
  fi
}

echo "render checks:"
harness "$TMP/c.html"  circle "renderCircle(document.getElementById('rings'), [CLAUDE], NOW);"
check   "$TMP/c.html"  '<i>5h</i>.*<i>wk</i>.*<i>left</i>' "circle: three rings"
check   "$TMP/c.html"  '<b>18%</b>'                         "circle: 5h shows 18%"
check   "$TMP/c.html"  '<b>3h</b>'                          "circle: countdown shows 3h"

harness "$TMP/d.html"  card "renderCard(document.getElementById('body'), [CLAUDE], NOW);"
check   "$TMP/d.html"  'class="row auth".*18%'              "card: authoritative 5h row"
check   "$TMP/d.html"  'class="row note".*wk 527.0M'       "card: totals note"

harness "$TMP/m.html"  card "renderCard(document.getElementById('body'), [CLAUDE, CODEX], NOW);"
check   "$TMP/m.html"  'class="adv critical"'               "card: critical advisory"

harness "$TMP/e.html"  card "renderCard(document.getElementById('body'), [], NOW);"
check   "$TMP/e.html"  'class="empty"'                       "card: empty state"

if [ "$CHECK_ONLY" = 1 ]; then echo "checks passed."; exit 0; fi

echo "screenshots:"
harness "$TMP/circle.html" circle "renderCircle(document.getElementById('rings'), [CLAUDE], NOW);"
shot    "$TMP/circle.html" "$OUT/circle.png"     220 620

harness "$TMP/card.html"   card   "renderCard(document.getElementById('body'), [CLAUDE], NOW);"
shot    "$TMP/card.html"   "$OUT/card.png"       620 460

harness "$TMP/multi.html"  card   "renderCard(document.getElementById('body'), [CLAUDE, CODEX], NOW);"
shot    "$TMP/multi.html"  "$OUT/card-multi.png" 620 620

echo "done."
