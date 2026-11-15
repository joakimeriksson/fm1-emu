import test from "node:test";
import assert from "node:assert/strict";
import {SaveStore} from "../persistence.js";
import {selectedInputs,panicMessages} from "../midi-input.js";
test("instances and setups cannot address the same flash database",async()=>{
  const names=[];
  const factory={open(name){names.push(name);const request={};queueMicrotask(()=>{request.error=new Error("test stop");request.onerror();});return request;}};
  for(const scope of ["studio:instance-a:setup-a","studio:instance-a:setup-b","studio:instance-b:setup-a"])
    await assert.rejects(new SaveStore(factory,scope).open());
  assert.equal(new Set(names).size,3);
  assert.throws(()=>new SaveStore(factory,"bad/path"),/namespace/);
});
test("same-named keyboards route by ID; ambiguous legacy names route nowhere",()=>{
  const ports=[{id:"usb-a",name:"Keyboard"},{id:"usb-b",name:"Keyboard"}];
  assert.deepEqual(selectedInputs(ports,"usb-b"),[ports[1]]);
  assert.deepEqual(selectedInputs(ports,"Keyboard"),[]);
  assert.deepEqual(selectedInputs(ports,""),[]);
  assert.deepEqual(selectedInputs(ports,"*"),ports);
});
test("panic releases sustain, notes and sound on all MIDI channels",()=>{
  const messages=panicMessages();assert.equal(messages.length,48);
  for(let channel=0;channel<16;channel++)
    assert.deepEqual(messages.slice(channel*3,channel*3+3),[64,123,120].map(cc=>[0xb0|channel,cc,0]));
});
