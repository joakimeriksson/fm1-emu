import {pack,unpack} from "./protocol.js";
export class MidiConnection {
  constructor(input,output,timeout=3000){
    this.input=input;this.output=output;this.timeout=timeout;this.pending=null;this.closed=false;
    this.receive=e=>{let packet;try{packet=unpack(e.data);}catch(error){this.close(error);return;}
      if(!packet||!this.pending)return;
      if(packet.command!==0&&packet.command!==this.pending.command)return;
      const p=this.pending;this.pending=null;clearTimeout(p.timer);
      if(packet.command===0){const e=Error(['','Bad request.','Device busy.','No saved data.','Checksum refused.','File refused.','Flash error.'][packet.bytes[0]]||'Device refused request.');e.code=packet.bytes[0];p.reject(e);}else p.resolve(packet.bytes);
    };
    input.addEventListener('midimessage',this.receive);
  }
  request(command,bytes=[]){
    if(this.closed||this.input.state==='disconnected'||this.output.state==='disconnected')return Promise.reject(Error('Device disconnected. Reconnect before trying again.'));
    if(this.pending)return Promise.reject(Error('A device request is already running.'));
    return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>this.close(Error('Device did not reply. Reconnect; no write was retried.')),this.timeout);
      this.pending={command,resolve,reject,timer};
      try{this.output.send(pack(command,bytes));}catch(error){this.close(error);}
    });
  }
  close(error=Error('Disconnected.')){this.closed=true;this.input.removeEventListener('midimessage',this.receive);if(this.pending){clearTimeout(this.pending.timer);this.pending.reject(error);this.pending=null;}}
}
