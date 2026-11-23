---
load_when: "always — project scope and upstream baseline"
last_verified: 2026-11-23
source_refs:
  - UPSTREAM_REVISION
  - crates/fm1-live/src/studio/**
  - AGENTS.md
---
# Overview

This branch rebuilds FM-1 Studio from upstream fm1-emu, retaining the original firmware execution model. The initial custom product is the Mac standalone instrument: complete app icon family and sources, approved graphite panel, native application menus and shortcuts, remembered launch settings and firmware flash, and the dedicated MiniLab 3 User program.

The previous Studio implementation stays on master as migration evidence until this branch is merged. Its browser Studio, hardware companion and custom plugin behavior are outside this scope. Upstream browser, plugins, diagnostic tools and other platforms remain in the tree; preserving them avoids needless divergence. They retain upstream behavior.

The user authorized this narrower scope and delivery through a branch and PR. Use the existing account-owned repository rather than creating another fork. UPSTREAM_REVISION records the exact foundation; never reset or force-push a published branch to update it.
