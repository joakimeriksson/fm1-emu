#!/usr/bin/env bash
# Local static build; never downloads firmware or installs plugins.
set -euo pipefail
cd "$(dirname "$0")/.."
web/build.sh
node tools/package-studio.mjs
