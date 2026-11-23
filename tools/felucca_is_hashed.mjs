// The Felucca fetched for the plugin's tests against the build tests/catalog.json hashed: the
// plugin's audio test compares only on that build, and plays any other without comparing (cargo
// hides what a passing test prints). Said here as a warning on the run, not failed: the build its
// author has published since is not this repository's doing, and the catalog check counts those.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
const root = new URL("..", import.meta.url);
const got = createHash("sha256").update(readFileSync(new URL("reference/firmwares/felucca.fwsc", root))).digest("hex");
const want = JSON.parse(readFileSync(new URL("tests/catalog.json", root))).felucca?.package;
if (want && got.startsWith(want)) console.log(`felucca.fwsc is the build the catalog hashed (${want})`);
else console.log(`::warning::felucca.fwsc as fetched (${got.slice(0, 16)}) is not the build tests/catalog.json hashed (${want}): ` +
  "the plugin's audio is not compared with the catalog in this run (refresh the catalog)");
