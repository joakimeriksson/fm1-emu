// Only a bounded audio queue runs on the realtime thread. No CPU emulation, JIT or file I/O.
class Fm1Buffer extends AudioWorkletProcessor {
  constructor() {
    super(); this.queue = new Array(16); this.head = 0; this.length = 0; this.pending = 0; this.seq = null;
    this.calmFrames = 0; this.last = [0, 0]; this.recovering = false;
    this.started = false; this.underruns = 0; this.target=4;
    this.port.onmessage = ({data:m}) => {
      if (m.type === 'connect-renderer') {
        this.renderer = m.port;
        this.renderer.onmessage = ({data:r}) => {
          if (r.type === 'audio') {
            this.pending = Math.max(0, this.pending - 1);
            if (r.seq === this.seq && r.playing && this.length < this.queue.length) {
              this.queue[(this.head + this.length) % this.queue.length] = r; this.length++;
            }
          } else {
            if (r.type === 'booted') { this.seq=r.seq; this.clear(); this.target=4; this.status(); }
            if (r.type === 'snapshot' && r.request !== undefined) {
              // A pause acknowledgement must silence already rendered audio too.
              if (this.pauseRequest === r.request) {this.clear();}
            }
            if (r.type === 'error') {
              if (r.fatal) {this.seq=null;this.clear();}
              else this.seq=this.previousSeq;
            }

          }
        };
        this.renderer.start();
      } else if (this.renderer) {
        if (m.type === 'boot' || m.type === 'cancel-boot') {this.previousSeq=this.seq;this.seq=null;this.clear();}
        if (m.type === 'snapshot' && m.pause) this.pauseRequest=m.request;
        this.renderer.postMessage(m);
      }
    };
  }
  clear() {
    this.queue.fill(null); this.head=0; this.length=0; this.started=false;
    this.calmFrames=0; this.last[0]=0; this.last[1]=0; this.recovering=false;
  }
  status() {
    this.renderer?.postMessage({type:'output-status',underruns:this.underruns,queueQuanta:this.target});
  }
  process(_inputs, outputs) {
    const out=outputs[0], n=out[0].length;
    if (!this.started && this.length >= this.target) this.started=true;
    if (this.started) {
      if (this.length) {
        const block=this.queue[this.head];
        this.queue[this.head]=null; this.head=(this.head+1)%this.queue.length; this.length--;
        out[0].set(block.l); if(out[1])out[1].set(block.r);
        if(this.recovering) {
          for(let i=0;i<Math.min(32,n);i++) {
            const gain=(i+1)/32; out[0][i]*=gain; if(out[1])out[1][i]*=gain;
          }
          this.recovering=false;
        }
        this.last[0]=out[0][n-1]; this.last[1]=out[1]?.[n-1] ?? this.last[0];
        this.calmFrames+=n;
        // Request less ahead and let the queue drain naturally; never drop valid samples.
        if(this.target>4 && this.calmFrames>=sampleRate*10) {
          this.target--; this.calmFrames=0; this.status();
        }
      } else {
        this.underruns++; this.target=Math.min(16,this.target+2); this.started=false;
        this.calmFrames=0; this.recovering=true;
        for(let i=0;i<Math.min(32,n);i++) {
          const gain=1-(i+1)/32; out[0][i]=this.last[0]*gain; if(out[1])out[1][i]=this.last[1]*gain;
        }
        this.last[0]=0; this.last[1]=0; this.status();
      }
    }
    if(this.renderer) {
      const count=Math.max(0,this.target-this.length-this.pending);
      if(count) {this.pending+=count;this.renderer.postMessage({type:'render-ahead',count});}
    }
    return true;
  }
}
registerProcessor('fm1',Fm1Buffer);
