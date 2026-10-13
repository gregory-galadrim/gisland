#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
export GISLAND_SCREENSHOTS="${GISLAND_SCREENSHOTS:-$PWD/target/design-screenshots}"
xvfb-run -a cargo test --locked render_design_fixtures -- --ignored
printf 'Native screenshots: %s\n' "$GISLAND_SCREENSHOTS"
