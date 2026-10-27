#!/usr/bin/env bash
# Generate example SVGs for the README gallery: svg + headless-Chromium PNG render.
# Chromium is a snap here, so every path must stay inside $HOME.
set -euo pipefail
cd "$(dirname "$0")"
BIN=./target/release/svg-gen

gen() { # gen <slug> <query> <mode>
  local slug="$1" q="$2" mode="${3:-scene}"
  echo "=== $slug ($mode)  <=  $q"
  "$BIN" "$q" --mode "$mode" -o "examples/$slug.svg" || { echo "FAIL $slug"; return 0; }
  chromium --headless=new --no-sandbox --disable-gpu --hide-scrollbars \
    --window-size=1024,768 --screenshot="examples/$slug.png" \
    "file://$PWD/examples/$slug.svg" >/dev/null 2>&1 || echo "render fail $slug"
}

gen bromo          "gunung bromo"    scene
gen peta-indonesia "peta indonesia"  scene
gen kapak          "kapak"           object
gen pisau          "pisau raut"      object
gen ornamen-dayak  "motif dayak"     object
ls -l examples/