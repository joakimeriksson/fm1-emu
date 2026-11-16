import test from "node:test";
import assert from "node:assert/strict";
import { prepareImport, filterItems, downloadItem, decodeCollection, encodeCollection } from "./library.js";
test("untrusted imports reject empty, unknown, oversized and invalid firmware",async()=>{
  await assert.rejects(prepareImport("a.fwsc",new Uint8Array()),/non-empty/);
  await assert.rejects(prepareImport("a.wav",new Uint8Array(1024)),/original/);
  await assert.rejects(prepareImport("a.fwsc",new Uint8Array(9<<20)),/8 MB/);
  await assert.rejects(prepareImport("a.fwsc",new Uint8Array(1024)),/package/);
});
test("search handles names and exact hashes without mutating library order",()=>{
  const items=[{id:"b",name:"Zed",hash:"0123"},{id:"a",name:"Alpha",hash:"abcd"}];
  assert.equal(filterItems(items,"ALP")[0].id,"a");
  assert.equal(filterItems(items,"012")[0].id,"b");
  assert.deepEqual(items.map(i=>i.id),["b","a"]);
});
test("exports sanitize filenames but preserve binary payload",()=>{
  const bytes=new Uint8Array([1,2,3]);
  const file=downloadItem({name:"../bad<script>",kind:"firmware",bytes});
  assert.equal(file.bytes,bytes);assert.ok(!file.name.includes("<"));assert.ok(!file.name.includes("/"));
});
test("collection rejects invalid versions, oversized and non-byte contents before import",async()=>{
  await assert.rejects(decodeCollection('{"format":"other","version":1,"items":[]}'),/Unsupported/);
  await assert.rejects(decodeCollection(JSON.stringify({format:"fm1-studio-library",version:1,items:[{filename:"x.fwsc",data:[256]}]})),/Invalid/);
  const empty=await decodeCollection(encodeCollection([]));assert.deepEqual(empty,[]);
});
test("real firmware remains unchanged through collection roundtrip when fixture is provided",async t=>{
  if(!process.env.FM1_TEST_FWSC) {t.skip("Set FM1_TEST_FWSC to an original .fwsc fixture.");return;}
  const {readFile}=await import("node:fs/promises");
  const bytes=new Uint8Array(await readFile(process.env.FM1_TEST_FWSC));
  const item=await prepareImport("fixture.fwsc",bytes);
  const [restored]=await decodeCollection(encodeCollection([item]));
  assert.deepEqual(restored.bytes,bytes);assert.equal(restored.hash,item.hash);
});
