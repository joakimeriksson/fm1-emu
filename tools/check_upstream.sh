#!/usr/bin/env bash
# Custom behavior belongs in the standalone adapters, not shared runtime sources.
set -euo pipefail
cd "$(dirname "$0")/.."
revision=$(cat UPSTREAM_REVISION)
git merge-base --is-ancestor "$revision" HEAD
git diff --exit-code "$revision" -- \
  crates/fm1-isa crates/fm1-core crates/fm1-soc crates/fm1-map \
  crates/fm1-emu crates/fm1-gdbstub crates/fm1-web crates/fm1-plugin web \
  crates/fm1-live/src/panel.rs crates/fm1-live/src/labels.rs \
  crates/fm1-live/src/fit.rs crates/fm1-live/src/wav.rs \
  crates/fm1-live/src/panel_smk.rs crates/fm1-live/src/exclusive.rs \
  ':(exclude)crates/fm1-plugin/build.rs' \
  ':(exclude)crates/fm1-plugin/wrap/CMakeLists.txt' \
  ':(exclude)crates/fm1-plugin/wrap/icon.rc.in'
printf '%s\n' 'PASS: shared upstream runtime sources match UPSTREAM_REVISION'
