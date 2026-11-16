import {Sloop3,crc32,projectFile,parseProjectFile} from './protocol.js';
import {MidiConnection} from './transport.js';
const el=id=>document.getElementById(id);
let access,connection,device,chosen,prepared,busy=false,releaseLock;
const status=s=>el('status').textContent=s;
const progress=n=>el('progress').value=n;
function controls(){
  for(const id of ['ports','input','output','connect'])el(id).disabled=busy||!!device||(id!=='ports'&&!access);
  el('disconnect').disabled=busy||!connection;
  el('backup').disabled=busy||!device;
  el('prepare').disabled=busy||!device||!chosen;
  el('restore').disabled=busy||!device||!prepared||!el('safe').checked;
  el('file').disabled=busy;el('name').disabled=busy;
}
function disconnect(){connection?.close();connection=null;device=null;prepared=null;el('plan').hidden=true;releaseLock?.();releaseLock=null;el('device').textContent='Not connected.';controls();}
async function run(fn){if(busy)return;busy=true;controls();progress(0);try{await fn();}catch(e){status(e.message);if(connection?.closed)disconnect();}finally{busy=false;controls();}}
function download(text,name){const url=URL.createObjectURL(new Blob([text],{type:'application/json'})),a=document.createElement('a');a.href=url;a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(url),30000);}
function filename(name){return (name.replace(/[^a-z0-9 _-]/gi,'_').slice(0,100)||'SLOOP project')+'.fm1project';}
const stamp=()=>new Date().toISOString().replace(/[:.]/g,'-');
el('ports').onclick=()=>run(async()=>{
  if(!navigator.requestMIDIAccess)throw Error('USB MIDI is unavailable. Open this page in Chrome or Edge.');
  if(!navigator.locks)throw Error('This browser cannot protect a transfer session. Use a current Chrome or Edge.');
  access=await navigator.requestMIDIAccess({sysex:true});
  for(const [id,ports] of [['input',access.inputs],['output',access.outputs]]){
    el(id).replaceChildren();for(const p of ports.values()){const o=document.createElement('option');o.value=p.id;o.textContent=p.name||p.id;el(id).append(o);}
  }
  access.onstatechange=()=>{if(connection&&(connection.input.state==='disconnected'||connection.output.state==='disconnected')){connection.close(Error('USB disconnected. Keep your safety backup; reconnect before another operation.'));disconnect();status('USB disconnected. Keep your safety backup; reconnect before another operation.');}};
  status('Choose the FM-1 input and output. Port names alone do not identify firmware.');
});
async function lock(){
  await new Promise((resolve,reject)=>{
    navigator.locks.request('fm1-hardware-library',{ifAvailable:true},async held=>{
      if(!held){reject(Error('Another Hardware Library tab is connected. Disconnect it first.'));return;}
      await new Promise(release=>{releaseLock=release;resolve();});
    }).catch(reject);
  });
}
el('connect').onclick=()=>run(async()=>{
  const input=access.inputs.get(el('input').value),output=access.outputs.get(el('output').value);
  if(!input||!output)throw Error('Choose both MIDI ports.');
  await lock();
  try{await input.open();await output.open();connection=new MidiConnection(input,output);device=new Sloop3((c,b)=>connection.request(c,b));await device.identify();
    el('device').textContent=`${input.name} → ${output.name} · SLOOP ${device.version}`;status('Connected to hardware. No device content has been changed.');
  }catch(e){disconnect();throw e;}
});
el('disconnect').onclick=()=>{disconnect();status('Disconnected.');};
el('backup').onclick=()=>run(async()=>{
  await device.stopped();const bytes=await device.read(2,progress),name=el('name').value.trim()||'My SLOOP project';
  download(projectFile(name,device.version,bytes),filename(name+' '+stamp()));status('Project downloaded. Keep the file and its samples in your backup folder.');
});
el('file').onchange=()=>run(async()=>{
  chosen=null;prepared=null;el('plan').hidden=true;el('selected').textContent='No project selected.';const file=el('file').files[0];if(!file)return;
  if(file.size>400000)throw Error('Project file is too large.');chosen=parseProjectFile(await file.text());
  el('selected').textContent=`${chosen.name} · SLOOP ${chosen.firmwareVersion} · ${chosen.bytes.length} bytes`;
  status('File checked. Preparing a restore will not write to hardware.');
});
el('prepare').onclick=()=>run(async()=>{
  prepared=null;el('plan').hidden=true;await device.identify();
  if(chosen.firmwareVersion!==device.version)throw Error('Firmware versions differ. Use the matching version before restoring.');
  await device.stopped();const safety=await device.read(2,progress);
  download(projectFile('Before restore',device.version,safety),filename('Before restore '+stamp()));
  prepared={crc:crc32(safety),input:connection.input.id,output:connection.output.id};el('safe').checked=false;
  el('summary').textContent=`Replace the project on ${connection.output.name} (SLOOP ${device.version}) with “${chosen.name}”. Samples, user sounds, firmware and calibration will not be written. A safety project backup has been downloaded. The project may need its original samples to sound right.`;
  el('plan').hidden=false;status('Check the destination and keep the safety backup before confirming.');
});
el('safe').onchange=controls;
el('restore').onclick=()=>run(async()=>{
  if(!prepared||!el('safe').checked)throw Error('Prepare and review the restore first.');
  const plan=prepared;prepared=null;el('plan').hidden=true;
  if(connection.input.id!==plan.input||connection.output.id!==plan.output)throw Error('Device changed. Prepare again.');
  await device.identify();await device.stopped();const current=await device.read(2);
  if(crc32(current)!==plan.crc)throw Error('Hardware project changed. Prepare again to keep a fresh backup.');
  status('Writing project. Keep USB connected until verification finishes.');
  try { await device.write(chosen.bytes,progress); } catch(e) { connection?.close(e); throw Error('Restore was not verified. Keep the safety backup and reconnect before trying again. '+e.message); }
  status('Project restored and read back successfully. Hardware sound verification is still pending.');
});
window.addEventListener('beforeunload',e=>{if(busy){e.preventDefault();e.returnValue='';}});
controls();
