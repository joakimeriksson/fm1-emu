import {test} from 'node:test';
import assert from 'node:assert/strict';
import {MiniLab3,isMiniLab3} from '../midi-input.js';
test('factory MiniLab knobs drive all four panel controls without forwarding',()=>{
 const p=new MiniLab3();
 for(const [cc,enc] of [[74,2],[71,3],[76,4],[77,5],[93,0],[18,6],[19,1]]) {
  assert.deepEqual(p.route([0xb0,cc,64]),[]);
  assert.deepEqual(p.route([0xb0,cc,66]),[{type:'enc',enc,clicks:2}]);
  assert.deepEqual(p.route([0xb0,cc,65]),[{type:'enc',enc,clicks:-1}]);
 }
 assert.deepEqual(p.route([0xb0,114,65]),[{type:'enc',enc:0,clicks:1}]);
 assert.deepEqual(p.route([0xb0,16,127]),[{type:'pot',value:1023}]);
 for(const message of [[0x90,60,100],[0x80,60,0],[0xb0,64,127],[0x99,36,100],[0xe0,0,64]]) assert.equal(p.route(message),null);
 assert.ok(isMiniLab3('Minilab3 MIDI'));assert.ok(!isMiniLab3('Minilab3 MCU/HUI'));
});

test('FM-1 relative layout responds immediately and never anchors or saturates',()=>{
 const p=new MiniLab3('fm1');
 for(const [cc,enc] of [[71,0],[76,6],[77,1],[93,2],[18,3],[19,4],[16,5],[114,0],[112,6]]) {
  for(const [value,clicks] of [[65,1],[65,1],[63,-1],[67,3]])
   assert.deepEqual(p.route([0xb0,cc,value]),[{type:'enc',enc,clicks}]);
  assert.deepEqual(p.route([0xb0,cc,64]),[]);
 }
 for(const [value,expected] of [[0,0],[127,1023]]) assert.deepEqual(p.route([0xb0,74,value]),[{type:'pot',value:expected}]);
});
test('both pad banks cover the panel, including FX, octave commands and SAVE',()=>{
 const p=new MiniLab3('fm1');
 for(const [i,id] of [8,2,6,11,3,12,13,7,4,5,10,9,0,1,8,3].entries()) {
  assert.deepEqual(p.route([0xbf,20+i,127]),[{type:'key',id,down:true}]);
  assert.deepEqual(p.route([0xbf,20+i,127]),[]);
  assert.deepEqual(p.route([0xbf,20+i,0]),[{type:'key',id,down:false}]);
 }
});
test('duplicate HOME holds release only after all sources let go; panic releases buttons',()=>{
 const p=new MiniLab3('fm1');
 assert.deepEqual(p.route([0xbf,20,127]),[{type:'key',id:8,down:true}]);
 for(const msg of [[0xbf,34,127],[0xb0,115,127],[0xbf,20,0],[0xbf,34,0]]) assert.deepEqual(p.route(msg),[]);
 assert.deepEqual(p.route([0xb0,123,0]),[{type:'key',id:8,down:false},{type:'midi',bytes:[0xb0,123,0]}]);
 assert.deepEqual(p.route([0xb0,115,0]),[]);
});
test('dedicated profile preserves musical notes, faders and pitch; Shift stays local',()=>{
 const p=new MiniLab3('fm1');
 for(const msg of [[0x90,60,100],[0x80,60,0],[0x99,36,100],[0xb0,20,127],[0xb0,17,127],[0xb0,82,127],[0xb0,83,127],[0xb0,85,127],[0xb0,64,127],[0xe0,0,64],[0xb0,1,127]]) assert.equal(p.route(msg),null);
 for(const cc of [9,113]) assert.deepEqual(p.route([0xb0,cc,127]),[]);
});
