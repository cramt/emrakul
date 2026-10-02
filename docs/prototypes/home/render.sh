#!/usr/bin/env -S nix shell nixpkgs#chromium nixpkgs#bash -c bash
# PROTOTYPE: renders every variant/state of index.html to a 3840x2160 PNG next to it.
set -eu
cd "$(dirname "$0")"
for v in ribbon grid list; do
  for s in recent empty; do
    chromium --headless=new --no-sandbox --hide-scrollbars --force-device-scale-factor=2 \
      --window-size=1920,1080 --virtual-time-budget=3000 \
      --screenshot="$PWD/$v-$s.png" "file://$PWD/index.html?v=$v&state=$s" 2>/dev/null
  done
done
