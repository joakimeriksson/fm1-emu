# Releases

One version for everything: the workspace's (`version` in `Cargo.toml`, which every crate takes)
and the plugin's (`crates/fm1-plugin/Cargo.toml`, a workspace of its own) are the same. A tag
`v<version>` releases it: `.github/workflows/release.yml` builds, tests and packages

- on macOS (Apple silicon): the plugin (AU, VST3, CLAP; Apple's AU validator and Steinberg's VST3
  validator) and fm1-live / fm1-emu;
- on Windows x86-64: the plugin (VST3, CLAP; Steinberg's VST3 validator), unsigned;
- on Linux arm64 and x86-64: fm1-live / fm1-emu;

smoke-tests each fm1-live on FM-1 SID's package, plays the whole catalog with the JIT and
interpreted against `tests/catalog.json` (`.github/workflows/catalog.yml`, which also runs on every
pull request that changes the emulator), and publishes one GitHub Release: the notes in
`docs/releases/v<version>.md`, then the pull requests merged since the last release (GitHub's
list). The page needs no release: it deploys from `master`.

The page shows the release from the same two places: `web/release.mjs`, run by `web/build.sh`,
writes the version into the header and a "What's new" section with the downloads from
`Cargo.toml`'s version and its notes: their first paragraph, the `## Downloads` table (file names
`fm1-plugin-<version>-<os>-<arch>.zip`, `fm1-emu-...`; "New." in a row marks it new) and the
`## What's new` list (each item shortened: the parentheses go; the "Contributors" item becomes
the thanks). `node --test web/*.test.mjs` fails on notes it cannot read. The release film: put
its YouTube link in the notes (a line such as `The release film: https://youtu.be/ID`) and the
page shows it in that section. `node web/release.mjs web/index.html web/smk37/index.html`
refreshes the copy committed in the pages (optional: the build writes its own).

## Steps

1. In a pull request: the version in `Cargo.toml` (`[workspace.package]`) and in
   `crates/fm1-plugin/Cargo.toml`; both `Cargo.lock`s follow (`cargo metadata` in the root and
   in `crates/fm1-plugin`); the notes, `docs/releases/v<version>.md` (what is new, for players:
   the merged pull requests are listed below them anyway).
2. Before merging: the pull request runs the release workflow (it changes the notes), which
   builds everything and keeps the files as the run's artifacts, no release. Download them
   from the run to try them.
3. Merge, then tag the merge on `master`:

   ```sh
   git checkout master && git pull
   git tag v0.2.0 && git push origin v0.2.0
   ```

The workflow refuses a tag that isn't the version, or a version without its notes.

By hand before the tag (not in CI): M-VAVE's stock firmware and FM1 Open, which are not in the
catalog, and the speed against the last release (CI machines are too noisy for it):
`tools/catalog_check.py` plays the catalog in about a minute on an Apple silicon Mac.

A change to the emulator that moves a firmware's sound (timing, an instruction's meaning)
updates the hashes in the same pull request, `tools/catalog_check.py --update`, and says which
firmwares changed and why.

Unsigned without the signing secrets (see crates/fm1-plugin/README.md); the notes and each
archive's README say how to let macOS open the downloads.
