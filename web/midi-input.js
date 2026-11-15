// Pure routing policy shared with controller regression tests.
export function selectedInputs(inputs, wanted) {
  if (!wanted) return [];
  if (wanted === "*") return inputs;
  const exact = inputs.filter(input => input.id === wanted);
  if (exact.length) return exact;
  const legacy = inputs.filter(input => input.name === wanted);
  return legacy.length === 1 ? legacy : [];
}
export function panicMessages() {
  return Array.from({length:16}, (_, channel) =>
    [64,123,120].map(controller => [0xb0 | channel,controller,0])).flat();
}

export function isMiniLab3(name = '') {
  const n=name.toLowerCase().replaceAll(' ','');
  return n.includes('minilab3') && n.endsWith('midi');
}
// Factory absolute CCs. Keep in step with fm1-soc/controller.rs; first touch only anchors.
export class MiniLab3 {
  previous = new Map();
  route(bytes) {
    if(bytes.length!==3 || (bytes[0]&0xf0)!==0xb0) return null;
    const [,cc,value]=bytes;
    const enc=new Map([[74,2],[71,3],[76,4],[77,5],[93,0],[18,6],[19,1]]).get(cc);
    if(enc!==undefined) {
      const last=this.previous.get(cc);this.previous.set(cc,value);
      return last===undefined || last===value ? [] : [{type:'enc',enc,clicks:value-last}];
    }
    if(cc===114 || cc===112) return value===64 ? [] : [{type:'enc',enc:cc===112?6:0,clicks:value-64}];
    if(cc===16 || cc===17) return [{type:'pot',value:Math.round(value*1023/127)}];
    if(cc===115 || cc===9) return [{type:'key',id:cc===9?3:8,down:value>=64}];
    return null;
  }
}
