// Only a bounded audio queue runs on the realtime thread. No CPU emulation, JIT or file I/O.
class Fm1Buffer extends AudioWorkletProcessor {
  constructor() {
    super(); this.queue = []; this.pending = 0; this.seq = null;
    this.started = false; this.underruns = 0; this.target=4;
    this.port.onmessage = ({data:m}) => {
      if (m.type === 'connect-renderer') {
        this.renderer = m.port;
        this.renderer.onmessage = ({data:r}) => {
          if (r.type === 'audio') {
            this.pending = Math.max(0, this.pending - 1);
            if (r.seq === this.seq && r.playing) this.queue.push(r);
          } else {
            if (r.type === 'booted') { this.seq=r.seq; this.queue=[]; this.started=false; }
            if (r.type === 'snapshot' && r.request !== undefined) {
              // A pause acknowledgement must silence already rendered audio too.
              if (this.pauseRequest === r.request) {this.queue=[];this.started=false;}
            }
            if (r.type === 'error') {
              if (r.fatal) {this.seq=null;this.queue=[];this.started=false;}
              else this.seq=this.previousSeq;
            }

          }
        };
        this.renderer.start();
      } else if (this.renderer) {
        if (m.type === 'boot' || m.type === 'cancel-boot') {this.previousSeq=this.seq;this.seq=null;this.queue=[];this.started=false;}
        if (m.type === 'snapshot' && m.pause) this.pauseRequest=m.request;
        this.renderer.postMessage(m);
      }
    };
  }
  process(_inputs, outputs) {
    const out=outputs[0];
    // Start with four quanta (11.6 ms at 44.1 kHz) to absorb worker/JIT scheduling jitter.
    if (!this.started && this.queue.length >= this.target) this.started=true;
    if (this.started) {
      const block=this.queue.shift();
      if (block) {out[0].set(block.l);if(out[1])out[1].set(block.r);}
      else {
        this.underruns++; this.target=Math.min(16,this.target+2); this.started=false;
        this.renderer?.postMessage({type:'output-status',underruns:this.underruns,queueQuanta:this.target});
      }
    }
    if(this.renderer) {
      const count=Math.max(0,this.target-this.queue.length-this.pending);
      if(count) {this.pending+=count;this.renderer.postMessage({type:'render-ahead',count});}
    }
    return true;
  }
}
registerProcessor('fm1',Fm1Buffer);
