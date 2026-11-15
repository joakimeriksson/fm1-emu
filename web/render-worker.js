// CPU execution, JIT compilation, snapshots and LCD copies belong off the audio callback.
import { Fm1 } from './render-runtime.js';
self.onmessage = ({data}) => {
  if (data.type !== 'connect') return;
  globalThis.sampleRate = data.sampleRate;
  const port = data.port;
  let outputStatus = {underruns:0, queueQuanta:4};
  const bridge = { postMessage: (m, transfer) => {
    if (['booted','snapshot','error'].includes(m.type)) {
      port.postMessage({type:m.type,seq:m.seq,request:m.request,fatal:m.fatal});
    }
    if (m.type === 'frame') {m.outputUnderruns=outputStatus.underruns;m.outputQueueMs=outputStatus.queueQuanta*128000/sampleRate;}
    self.postMessage(m, transfer || []);
  }};
  const engine = new Fm1(bridge);
  port.onmessage = e => {
    const m = e.data;
    if (m.type === 'output-status') {outputStatus=m;}
    else if (m.type === 'render-ahead') {
      for (let i = 0; i < m.count; i++) {
        const l = new Float32Array(128), r = new Float32Array(128);
        engine.process([], [[l, r]]);
        port.postMessage({type:'audio', seq:engine.seq, l, r, playing:engine.playing && !engine.paused}, [l.buffer,r.buffer]);
      }
    } else bridge.onmessage(e);
  };
  port.start();
};
