import test from 'node:test';
import assert from 'node:assert/strict';
import {Sloop3,pack,unpack,crc32,w32,projectFile,parseProjectFile} from './protocol.js';
import {MidiConnection} from './transport.js';
const project=()=>{const b=new Uint8Array(800);b.set([71,66,80,74,15]);return b;};
test('SysEx roundtrip covers all byte values; malformed packets reject',()=>{
 const b=Uint8Array.from({length:256},(_,i)=>i);assert.deepEqual(unpack(pack(5,b)),{command:5,bytes:b});
 assert.equal(unpack(new Uint8Array([240,1,2,3,4,247])),null);
 const bad=pack(5,[1]);bad[5]=128;assert.throws(()=>unpack(bad));
 assert.equal(crc32(new TextEncoder().encode('123456789')),0xcbf43926);
});
test('project envelope preserves bytes, firmware version and checksums; corrupt files reject',()=>{
 const b=project(),file=projectFile('Song','3.3.7',b),r=parseProjectFile(file);
 assert.deepEqual(r.bytes,b);assert.equal(r.firmwareVersion,'3.3.7');
 const bad=JSON.parse(file);bad.bytes[16]=1;assert.throws(()=>parseProjectFile(JSON.stringify(bad)),/checksum/);
 bad.bytes[16]=256;assert.throws(()=>parseProjectFile(JSON.stringify(bad)),/Unsupported/);
 assert.throws(()=>projectFile('bad','3.3.7',new Uint8Array(20)),/supported/);
});
function simulator(){let saved=project(),staging,got=0,writes=0;return {get saved(){return saved},get writes(){return writes},async request(c,b=[]){
 if(c===1)return new Uint8Array([2,5,48,0,1,0,0,0,0,0,...new TextEncoder().encode('3.3.7'),0]);
 if(c===15)return new Uint8Array([0,0]);
 if(c===5){const off=b[2]|b[3]<<8|b[4]<<16|b[5]<<24;return new Uint8Array([b[0],0,...w32(off),...w32(saved.length),...w32(crc32(saved)),...saved.slice(off,off+384)]);}
 if(c===6){writes++;got=0;staging=new Uint8Array(b[2]|b[3]<<8|b[4]<<16|b[5]<<24);return new Uint8Array(b);}
 if(c===7){const off=b[0]|b[1]<<8|b[2]<<16|b[3]<<24;assert.equal(off,got);staging.set(b.slice(4),off);got+=b.length-4;return new Uint8Array([...w32(off),...w32(got)]);}
 if(c===8){assert.equal(got,staging.length);assert.deepEqual(b,w32(crc32(staging)));saved=staging;return new Uint8Array(b);}
 throw Error('Unknown command');
 }};}
test('simulated project backup, chunked restore and read-back verification',async()=>{
 const sim=simulator(),adapter=new Sloop3(sim.request.bind(sim));assert.equal(await adapter.identify(),'3.3.7');await adapter.stopped();
 assert.deepEqual(await adapter.read(),project());const next=project();next[799]=127;await adapter.write(next);assert.deepEqual(sim.saved,next);assert.equal(sim.writes,1);
});
test('unknown firmware, playback and corrupt/out-of-order chunks are rejected',async()=>{
 const sim=simulator();const unknown=new Sloop3(async()=>new Uint8Array([2,5,48,0,0,0,0,0,0,0,57,0]));await assert.rejects(unknown.identify(),/supports/);
 await assert.rejects(new Sloop3(async()=>new Uint8Array([0,1])).stopped(),/Stop/);
 const adapter=new Sloop3(async(c,b)=>{const r=await sim.request(c,b);if(c===5)r[2]=10;return r;});await assert.rejects(adapter.read(),/Unexpected/);
 const corrupt=new Sloop3(async(c,b)=>{const r=await sim.request(c,b);if(c===5)r[r.length-1]^=1;return r;});await assert.rejects(corrupt.read(),/checksum/);
});
test('MIDI ignores other command replies; disconnect rejects pending operation',async()=>{
 const input=new EventTarget();input.state='connected';const output={state:'connected',send(){}};const c=new MidiConnection(input,output,100);
 const p=c.request(1);const event=new Event('midimessage');event.data=pack(5,[1]);input.dispatchEvent(event);assert.ok(c.pending);
 c.close(Error('USB disconnected'));await assert.rejects(p,/disconnected/);await assert.rejects(c.request(1),/disconnected/);
});
test('timeout poisons session; commands are never blindly resent',async()=>{
 let sends=0;const i=new EventTarget();i.state='connected';const c=new MidiConnection(i,{state:'connected',send(){sends++;}},5);
 await assert.rejects(c.request(6,[1]),/Reconnect/);await assert.rejects(c.request(6,[1]));assert.equal(sends,1);
});
test('device refusal is surfaced with an explicit error code',async()=>{
 const i=new EventTarget();i.state='connected';const c=new MidiConnection(i,{state:'connected',send(){queueMicrotask(()=>{const e=new Event('midimessage');e.data=pack(0,[5]);i.dispatchEvent(e);});}},100);
 await assert.rejects(c.request(6),e=>e.code===5);c.close();
});

test('identity and stopped checks gate writes; malformed acknowledgements stop transfer',async()=>{
 let writes=0;
 const wrong=new Sloop3(async c=>{if(c===6)writes++;return new Uint8Array([2,5,48,0,0,0,0,0,0,0,57,0]);});
 await assert.rejects(wrong.write(project()),/supports/);assert.equal(writes,0);
 const sim=simulator(),calls=[];
 const bad=new Sloop3(async(c,b)=>{calls.push(c);if(c===6)return new Uint8Array(6);return sim.request(c,b);});
 await assert.rejects(bad.write(project()),/acknowledgement/);assert.ok(!calls.includes(7));
});
