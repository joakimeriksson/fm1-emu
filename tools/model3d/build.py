"""Original procedural FM-1 emulator geometry. No product image required."""
from pathlib import Path
import numpy as np
import trimesh as t
from PIL import Image, ImageDraw, ImageFont
P=Path(__file__).parent
W,H=800,550
S=3
s=.00025
scene=t.Scene()
knobs=[('master',119,125),('select',196,125),('presets',119,200),('algorithm',196,200),('knob1',469,125),('knob2',551,125),('knob3',633,125),('knob4',715,125)]
buttons=[(a,x,y,a.upper().replace('_','/')) for y,names in [(203,['fx','sel','env','lfo','edit','glo']),(249,['home','save','arp','seq','play_stop','rec'])] for a,x in zip(names,[478,524,570,616,662,708])]
buttons += [('oct_down',130,260,'OCT-'),('oct_up',187,260,'OCT+')]
keys=[(f'key_lower_{i:02}',x,417) for i,x in enumerate([115,156,197,238,279,320,361,402,443,484,525,566,607,648,689,730])]+[(f'key_upper_{i:02}',x,348) for i,x in enumerate([135,176,218,300,341,423,464,505,587,628,710])]
im=Image.new('RGB',(W*S,H*S),(52,54,57)); d=ImageDraw.Draw(im)
def rr(cx,cy,w,h,r,c,outline=None):
    d.rounded_rectangle(tuple(round(v*S) for v in (cx-w/2,cy-h/2,cx+w/2,cy+h/2)),radius=round(r*S),fill=c,outline=outline,width=S)
def label(x,y,txt,size=9):
    f=ImageFont.truetype('DejaVuSans.ttf',round(size*S)); d.text((x*S,y*S),txt,font=f,fill=(222,224,225),anchor='mm')
rr(423,273,722,432,43,(56,58,61),(110,112,114))
rr(423,273,712,422,39,(56,58,61),(39,41,43))
rr(423,383,678,160,27,(28,30,32),(14,15,17))
rr(593,226,295,103,12,(25,27,29),(13,14,16))
rr(158,260,115,38,8,(25,27,29),(13,14,16))
rr(333,176,174,174,15,(12,13,15),(8,9,10))
for name,x,y in knobs:
    label(x,y-32,name.upper(),9); rr(x,y,36,36,18,(19,20,22)); rr(x,y,32,32,16,(39,41,44),(80,82,85)); rr(x,y-9,3,8,1,(217,219,220))
for name,x,y,txt in buttons:
    w,h=(43,26) if name.startswith('oct') else (36,36)
    rr(x,y,w,h,6,(42,44,47),(9,10,12))
    if name=='play_stop': label(x,y-5,'PLAY',7); label(x,y+5,'STOP',7)
    else: label(x,y,txt,9)
for name,x,y in keys: rr(x,y,32,64,15,(36,38,41),(8,9,10))
im.save(P/'panel.png',optimize=True)
mat=t.visual.material.PBRMaterial(name='original_panel_atlas',baseColorTexture=im,roughnessFactor=.85,metallicFactor=0)
def solid(name,c,em=None): return t.visual.material.PBRMaterial(name=name,baseColorFactor=c,emissiveFactor=em,roughnessFactor=.8,metallicFactor=0)
def xy(x,y):return ((x-423)*s,(273-y)*s)
def poly(cx,cy,w,h,r):
    pts=[]
    for x,y,a in [(cx+w/2-r,cy-h/2+r,-90),(cx+w/2-r,cy+h/2-r,0),(cx-w/2+r,cy+h/2-r,90),(cx-w/2+r,cy-h/2+r,180)]:
        for k in range(7):
            ang=np.deg2rad(a+k*90/6);pts.append((x+r*np.cos(ang),y+r*np.sin(ang)))
    return np.array(pts)[::-1]
def add(name,cx,cy,w,h,r,z0,z1,color=(38,40,42,255),topmat=None,parent=None):
    p=poly(cx,cy,w,h,r); n=len(p); center=np.array(xy(cx,cy)); local=np.array([xy(*q) for q in p])-center
    verts=np.vstack([np.c_[local,np.full(n,z0)],np.c_[local,np.full(n,z1)],[[0,0,z0]]]); faces=[]
    for i in range(n):
        j=(i+1)%n;faces += [[i,j,n+j],[i,n+j,n+i],[2*n,j,i]]
    shell=t.Trimesh(verts,faces,process=False);shell.visual.face_colors=color
    v=np.vstack([np.c_[local,np.full(n,z1+.00002)],[[0,0,z1+.00002]]]); f=[[n,i,(i+1)%n] for i in range(n)]
    top=t.Trimesh(v,f,process=False);uv=np.vstack([p,[cx,cy]])/[W,H];uv[:,1]=1-uv[:,1]
    top.visual=t.visual.texture.TextureVisuals(uv=uv,material=topmat or mat)
    tr=np.eye(4);tr[:2,3]=center if parent is None else [0,0]
    scene.add_geometry(top,node_name=name,geom_name=name,transform=tr,parent_node_name=parent)
    scene.add_geometry(shell,node_name=name+'_shell',geom_name=name+'_shell',parent_node_name=name)
add('enclosure',423,273,722,432,43,-.023,0,(48,48,49,255))
for name,x,y in knobs: add('knob_'+name,x,y,32,32,16,0,.006)
for name,x,y,txt in buttons:
    w,h=(43,26) if name.startswith('oct') else (36,36)
    add('button_'+name,x,y,w,h,6,0,.003)
for name,x,y in keys:
    add(name,x,y,32,64,15,0,.0015)
    add(name+'_led',x,y,3.5,32,1.7,.00153,.00165,topmat=solid(name+'_emissive',[.32,.34,.36,1],[.08,.08,.08]),parent=name)
# True square, four vertices, full UV range; glTF exports with V flipped.
a=156*s/2; z=.0005
lcd=t.Trimesh(vertices=[[-a,-a,z],[a,-a,z],[a,a,z],[-a,a,z]],faces=[[0,1,2],[0,2,3]],process=False)
lcd.visual=t.visual.texture.TextureVisuals(uv=np.array([[0,0],[1,0],[1,1],[0,1]],dtype=float),material=solid('lcd_black',[0,0,0,1]))
tr=np.eye(4);tr[:2,3]=xy(333,176);scene.add_geometry(lcd,node_name='lcd',geom_name='lcd',transform=tr)
scene.export(P/'fm1.glb')
print('GLB bytes:',(P/'fm1.glb').stat().st_size)
