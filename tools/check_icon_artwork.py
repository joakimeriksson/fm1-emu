from pathlib import Path
import zlib,struct
root=Path(__file__).resolve().parents[1]
p=root/'assets/icons/source.png';b=p.read_bytes();o=8;raw=b''
while o<len(b):
 n=struct.unpack('>I',b[o:o+4])[0];k=b[o+4:o+8];d=b[o+8:o+8+n];o+=n+12
 if k==b'IHDR':w,h,depth,typ,*_=struct.unpack('>IIBBBBB',d)
 if k==b'IDAT':raw+=d
assert(depth,typ)==(8,6)
buf=zlib.decompress(raw);rows=[];prev=bytearray(w*4)
def paeth(a,b,c):
 p=a+b-c; ds=[abs(p-a),abs(p-b),abs(p-c)];return [a,b,c][ds.index(min(ds))]
for y in range(h):
 off=y*(w*4+1);f=buf[off];row=bytearray(buf[off+1:off+1+w*4])
 for i in range(len(row)):
  a=row[i-4] if i>=4 else 0;b=prev[i];c=prev[i-4] if i>=4 else 0
  row[i]=(row[i]+[0,a,b,(a+b)//2,paeth(a,b,c)][f])&255
 rows.append(row);prev=row
for x,y in [(10,10),(512,10),(1000,10),(10,512),(1000,512),(10,1000),(512,1000)]:assert list(rows[y][x*4:x*4+4])==[41,42,43,255]
for y in range(280,420,20):
 for x in range(330,480,20):assert max(rows[y][x*4:x*4+3])<20,'screen not blank'
print('PASS: exact uniform #292a2b body pixels and blank unlit screen')

import json
icon=root/'assets/icons/FM1.icon'
settings=json.loads((icon/'icon.json').read_text())
assert settings['groups'][0]['layers'][0]['glass'] is False
assert (icon/'Assets/device.png').read_bytes()==p.read_bytes()
print('PASS: native icon uses full-bleed composition without a baked glass layer')
