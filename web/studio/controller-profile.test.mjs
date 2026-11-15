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
