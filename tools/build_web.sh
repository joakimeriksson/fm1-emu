#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
web/build.sh
python3 tools/package_web_icons.py
