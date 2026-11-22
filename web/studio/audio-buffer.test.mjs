import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
function setup() {
 let Processor;
 const messages=[];
 class Base {constructor(){this.port={postMessage:m=>messages.push(m)};}}
 vm.runInNewContext(readFileSync(new URL('../worklet.js',import.meta.url),'utf8'),{
   sampleRate:44100,AudioWorkletProcessor:Base,registerProcessor:(_name,p)=>Processor=p});
 const p=new Processor(), requests=[];
 const renderer={postMessage:m=>requests.push(m),start(){}};
 p.port.onmessage({data:{type:'connect-renderer',port:renderer}});
 const reply=data=>renderer.onmessage({data});
 const audio=(seq,v)=>reply({type:'audio',seq,playing:true,l:new Float32Array(128).fill(v),r:new Float32Array(128).fill(-v)});
 const tick=()=>{const out=[new Float32Array(128),new Float32Array(128)];p.process([], [out]);return out;};
 return {p,reply,audio,tick,requests,messages};
}
test('bounded prefill absorbs worker delay and preserves stereo order',()=>{
 const s=setup();s.reply({type:'booted',seq:1});
 assert.equal(s.tick()[0][0],0);assert.equal(s.requests.find(m=>m.type==='render-ahead').count,4);
 s.audio(1,.1);s.audio(1,.2);s.audio(1,.3);
 assert.equal(s.tick()[0][0],0);assert.equal(s.requests.filter(m=>m.type==='render-ahead').length,1);
 s.audio(1,.4);const out=s.tick();assert.ok(Math.abs(out[0][0]-.1)<1e-6);assert.equal(out[0][0],-out[1][0]);
 assert.equal(s.requests.at(-1).count,1);
 s.tick();s.tick();s.tick();assert.equal(s.p.underruns,0);
 s.tick();assert.equal(s.p.underruns,1);
});
test('firmware replacement rejects old queued and in-flight audio',()=>{
 const s=setup();s.reply({type:'booted',seq:1});s.tick();
 s.p.port.onmessage({data:{type:'boot',seq:2}});
 for(let i=0;i<4;i++)s.audio(1,.9);
 assert.equal(s.tick()[0][0],0);
 s.reply({type:'booted',seq:2});for(let i=0;i<4;i++)s.audio(2,.2);
 assert.ok(Math.abs(s.tick()[0][0]-.2)<1e-6);
});
test('pause snapshot flushes buffered audio',()=>{
 const s=setup();s.reply({type:'booted',seq:1});for(let i=0;i<4;i++)s.audio(1,.7);s.tick();
 s.p.port.onmessage({data:{type:'snapshot',request:9,pause:true}});
 s.reply({type:'snapshot',request:9});assert.equal(s.tick()[0][0],0);
});

test('recovery fades both channels and counts one underrun while refilling',()=>{
 const s=setup();s.reply({type:'booted',seq:1});
 for(let i=0;i<4;i++)s.audio(1,.8);
 for(let i=0;i<4;i++)s.tick();
 const fade=s.tick();assert.ok(fade[0][0]<.8 && fade[0][0]>.7);
 assert.equal(fade[0][31],0);assert.equal(Math.abs(fade[1][31]),0);
 for(let i=0;i<20;i++)assert.equal(s.tick()[0][0],0);
 assert.equal(s.p.underruns,1);assert.equal(s.p.target,6);
 for(let i=0;i<6;i++)s.audio(1,.8);
 const resumed=s.tick();assert.ok(resumed[0][0]>.02 && resumed[0][0]<.03);
 assert.equal(resumed[0][0],-resumed[1][0]);
});
test('a stable stream recovers its original latency without dropping audio',()=>{
 const s=setup();s.reply({type:'booted',seq:1});s.p.target=6;
 for(let i=0;i<6;i++)s.audio(1,.2);
 for(let i=0;i<Math.ceil(44100*21/128);i++){s.tick();s.audio(1,.2);}
 assert.equal(s.p.target,4);assert.equal(s.p.underruns,0);
 assert.ok(s.p.length<=s.p.queue.length);
 s.reply({type:'booted',seq:2});assert.equal(s.p.target,4);assert.equal(s.p.length,0);
});
