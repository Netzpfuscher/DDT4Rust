#!/usr/bin/env bash
# Launch DDTRust's GUI with the simulator and a representative ECU screen.
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run -p ddtrust-gui --release -- \
  --simulate data/ecu \
  --ecu UCH_84_J84_05_00 \
  --screen "SSPP Options"
