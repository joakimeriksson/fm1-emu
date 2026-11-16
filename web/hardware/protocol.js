// Independent client for SLOOP 3's published app/edproto.h wire contract.
// Protocol reference and tested release are recorded in context/hardware-library.md.
export const MAX_PROJECT = 52992;
export const u32 = (b,o=0) => new DataView(b.buffer,b.byteOffset,b.byteLength).getUint32(o,true);
export const w32 = n => [n&255,(n>>>8)&255,(n>>>16)&255,n>>>24];
export function crc32(bytes) {
  let c=0xffffffff;
  for (const b of bytes) { c^=b; for(let i=0;i<8;i++) c=(c>>>1)^((c&1)?0xedb88320:0); }
  return (c^0xffffffff)>>>0;
}
export function pack(command, bytes=[]) {
  const out=[240,125,83,51,command]; let bits=0,acc=0;
  for(const b of bytes) {acc|=b<<bits;bits+=8;while(bits>=7){out.push(acc&127);acc>>>=7;bits-=7;}}
  if(bits)out.push(acc&127);out.push(247);return new Uint8Array(out);
}
export function unpack(data) {
  if(data.length<6 || data[0]!==240 || data[1]!==125 || data[2]!==83 || data[3]!==51 || data.at(-1)!==247) return null;
  const out=[];let bits=0,acc=0;
  for(const b of data.slice(5,-1)){if(b>127)throw Error('Invalid MIDI packet.');acc|=b<<bits;bits+=7;if(bits>=8){out.push(acc&255);acc>>>=8;bits-=8;}}
  return {command:data[4],bytes:new Uint8Array(out)};
}
export function validateProject(bytes) {
  if(!(bytes instanceof Uint8Array)||bytes.length<16||bytes.length>MAX_PROJECT||u32(bytes)!==0x4a504247||bytes[4]<1||bytes[4]>15)
    throw Error('This is not a supported SLOOP 3 project.');
  return bytes;
}
export function projectFile(name,version,bytes) {
  validateProject(bytes);
  return JSON.stringify({format:'fm1-hardware-project',version:1,firmware:'sloop3',firmwareVersion:version,name,crc32:crc32(bytes),bytes:Array.from(bytes)});
}
export function parseProjectFile(text) {
  if(text.length>400000)throw Error('Project file is too large.');
  const r=JSON.parse(text);
  if(r?.format!=='fm1-hardware-project'||r.version!==1||r.firmware!=='sloop3'||typeof r.firmwareVersion!=='string'||typeof r.name!=='string'||r.name.length>128||!Array.isArray(r.bytes)||r.bytes.length>MAX_PROJECT||r.bytes.some(b=>!Number.isInteger(b)||b<0||b>255))throw Error('Unsupported hardware project file.');
  r.bytes=validateProject(new Uint8Array(r.bytes));
  if(crc32(r.bytes)!==r.crc32)throw Error('Project checksum failed.');
  return r;
}
export class Sloop3 {
  constructor(request){this.request=request;}
  async ask(command,data=[]){
    // Only a definite BUSY reply may be retried; timeout/disconnect is never retried.
    for(let i=0;;i++){try{return await this.request(command,data);}catch(e){if(e.code!==2||i===40)throw e;await new Promise(r=>setTimeout(r,250));}}
  }
  async identify(){
    const b=await this.ask(1);
    if(b.length<11||b[0]!==2||b[1]!==5)throw Error('Not a supported SLOOP 3 device.');
    const version=new TextDecoder().decode(b.slice(10)).replace(/\0.*$/s,'');
    // Capability fence: only the release whose layout/protocol was inspected may write.
    if(version!=='3.3.7')throw Error(`SLOOP ${version}: this companion currently supports 3.3.7 only.`);
    this.version=version;return version;
  }
  async stopped(){const b=await this.ask(15,[0,0,0,0]);if(b.length<2||b[0]!==0)throw Error('Cannot check playback.');if(b[1]&7)throw Error('Stop playback and recording on the FM-1 first.');}
  async read(kind=2,progress=()=>{}){
    let data=null,crc,total,off=0;
    do{
      const b=await this.ask(5,[kind,0,...w32(off),128,1]);
      if(b.length<14||b[0]!==kind||b[1]!==0||u32(b,2)!==off)throw Error('Unexpected project chunk.');
      const n=u32(b,6),check=u32(b,10),part=b.slice(14);
      if(n<16||n>MAX_PROJECT||!part.length||part.length>384||off+part.length>n)throw Error('Invalid project length.');
      if(!data){total=n;crc=check;data=new Uint8Array(n);}else if(total!==n||crc!==check)throw Error('Project changed during backup. Stop editing and try again.');
      data.set(part,off);off+=part.length;progress(off/total);
    }while(off<total);
    if(crc32(data)!==crc)throw Error('Device backup checksum failed.');
    return validateProject(data);
  }
  async write(bytes,progress=()=>{}){
    validateProject(bytes);
    await this.identify();
    await this.stopped();
    const begin=await this.ask(6,[0,0,...w32(bytes.length)]);
    if(begin.length!==6||begin[0]!==0||begin[1]!==0||u32(begin,2)!==bytes.length)throw Error('Unexpected write acknowledgement.');
    for(let off=0;off<bytes.length;off+=384){
      const part=bytes.slice(off,off+384),b=await this.ask(7,[...w32(off),...part]);
      if(b.length!==8||u32(b)!==off||u32(b,4)!==off+part.length)throw Error('Unexpected write offset.');
      progress((off+part.length)/bytes.length);
    }
    const checksum=crc32(bytes),end=await this.ask(8,w32(checksum));
    if(end.length!==4||u32(end)!==checksum)throw Error('Unexpected commit acknowledgement.');
    const check=await this.read(0);
    if(check.length!==bytes.length||check.some((v,i)=>v!==bytes[i]))throw Error('Read-back verification failed. Keep the safety backup.');
  }
}
