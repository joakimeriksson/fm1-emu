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
// Keep both profiles in step with fm1-soc/controller.rs; factory absolute controls anchor first.
export class MiniLab3 {
  previous = new Map();
  held = Array(17).fill(false);
  constructor(mode = 'factory') { this.dedicated = mode === 'fm1'; }
  key(source, down) {
    const ids=[8,2,6,11,3,12,13,7,4,5,10,9,0,1,8,3,8], id=ids[source];
    const active=()=>this.held.some((held,i)=>held && ids[i]===id);
    const before=active(); this.held[source]=down;
    return before===active() ? [] : [{type:'key',id,down:active()}];
  }
  route(bytes) {
    if(bytes.length!==3 || (bytes[0]&0xf0)!==0xb0 || bytes[1]>127 || bytes[2]>127) return null;
    const [,cc,value]=bytes;
    if(this.dedicated) {
      if(cc===74) return [{type:'pot',value:Math.round(value*1023/127)}];
      const enc=new Map([[71,0],[76,6],[77,1],[93,2],[18,3],[19,4],[16,5],[114,0],[112,6]]).get(cc);
      if(enc!==undefined) return value===64 ? [] : [{type:'enc',enc,clicks:value-64}];
      if(bytes[0]===0xbf && cc>=20 && cc<36) return this.key(cc-20,value>=64);
      if(cc===115) return this.key(16,value>=64);
      if(cc===9 || cc===113) return [];
      if(cc===120 || cc===123) {
        // Return release actions plus the original panic MIDI message.
        return this.held.flatMap((_,i)=>this.key(i,false)).concat([{type:'midi',bytes}]);
      }
      return null;
    }
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
