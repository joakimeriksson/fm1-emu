"""Original FM1 geometry and artwork; metres, +Z front. pip install trimesh pillow numpy"""
from pathlib import Path
import numpy as np
import trimesh as t
from PIL import Image, ImageDraw, ImageFont, ImageFilter
P=Path(__file__).parent; W,H=800,620; S=3; s=.00025
scene=t.Scene()
knobs=[('master',98,113),('select',168,113),('presets',98,203),('algorithm',168,203),('knob1',542,113),('knob2',610,113),('knob3',678,113),('knob4',746,113)]
buttons=[(a,x,y,a.upper().replace('_','/')) for y,names in [(200,['fx','sel','env','lfo','edit','glo']),(253,['home','save','arp','seq','play_stop','rec'])] for a,x in zip(names,[538,580,622,664,706,748])]+[('oct_down',101,269,'OCT-'),('oct_up',160,269,'OCT+')]
keys=[(f'key_lower_{i:02}',x,439) for i,x in enumerate([115,156,197,238,279,320,361,402,443,484,525,566,607,648,689,730])]+[(f'key_upper_{i:02}',x,370) for i,x in enumerate([135,176,218,300,341,423,464,505,587,628,710])]
def mat(n,c,r=.5,m=0,e=None): return t.visual.material.PBRMaterial(name=n,baseColorFactor=[*map(float,c),1.],roughnessFactor=r,metallicFactor=m,emissiveFactor=e)
body=mat('graphite powder coat',(.15,.16,.17),.66,.18); recess=mat('recessed black polymer',(.035,.038,.042),.76); rubber=mat('soft touch caps',(.11,.12,.13),.48); metal=mat('anodized knob',(.22,.23,.24),.32,.65); rim=mat('knob bevel',(.31,.32,.33),.27,.65); ink=mat('indicator',(.8,.82,.83),.5); glass=mat('black glass bezel',(.025,.028,.032),.18,.12)
def xy(x,y): return np.array([(x-423)*s,(256-y)*s])
def outline(w,h,r,n=8):
    return np.array([(cx+r*np.cos(a),cy+r*np.sin(a)) for cx,cy,start in [(w/2-r,h/2-r,0),(-w/2+r,h/2-r,90),(-w/2+r,-h/2+r,180),(w/2-r,-h/2+r,270)] for a in np.deg2rad(np.linspace(start,start+90,n,endpoint=False))])
def mesh(name,v,f,material,center=None,parent=None,uv=None):
    g=t.Trimesh(vertices=v,faces=f,process=False); g.visual=t.visual.texture.TextureVisuals(uv=np.zeros((len(v),2)) if uv is None else uv,material=material)
    tr=np.eye(4); tr[:2,3]=xy(*center) if center is not None else [0,0]; scene.add_geometry(g,node_name=name,geom_name=name,transform=tr,parent_node_name=parent); return g
# Beveled rings, with separate flat top material if desired.
def shape(name,c,w,h,r,z0,z1,b,material,topmat=None,parent=None,topname=None,atlas=False,local_uv=False):
    b=min(b,r*.8,(z1-z0)*.45/s); rings=[(b,z0),(0,z0+b*s),(0,z1-b*s),(b,z1)]
    p=[outline(w-2*d,h-2*d,max(r-d,.2))*s for d,z in rings]; n=len(p[0]); v=np.vstack([np.c_[q,np.full(n,z)] for q,(d,z) in zip(p,rings)]+[[0,0,z0]]); f=[]
    for k in range(3):
        for i in range(n):
            j=(i+1)%n; a=k*n+i; q=k*n+j; f.extend([[a,q,q+n],[a,q+n,a+n]])
    f.extend([[4*n,(i+1)%n,i] for i in range(n)])
    mesh(name,v,f,material,c,parent)
    top=np.vstack([np.c_[p[-1],np.full(n,z1)],[[0,0,z1]]]); uv=None
    if atlas:
        uv=np.c_[(top[:,0]/s+c[0])/W,1-(c[1]-top[:,1]/s)/H]
    if local_uv:
        uv=np.c_[top[:,0]/((w-2*b)*s)+.5,top[:,1]/((h-2*b)*s)+.5]
    mesh(topname or name+'_top',top,[[n,i,(i+1)%n] for i in range(n)],topmat or material,parent=name,uv=uv)
# Original printed artwork, no baked fake controls or photos.
rng=np.random.default_rng(42)
# Multi-scale original graphite grain, with subtle variations in reflectance.
fine=rng.normal(0,1,(H*S,W*S)); coarse=np.asarray(Image.fromarray(np.uint8(np.clip(128+rng.normal(0,24,(H//3,W//3)),0,255))).resize((W*S,H*S),Image.Resampling.BICUBIC),dtype=float)-128
arr=np.clip((53+fine*3.5+coarse*.075)[:,:,None]+np.array([0,1,2]),0,255).astype('uint8'); im=Image.fromarray(arr); d=ImageDraw.Draw(im)
def label(x,y,text,size=9,color=(220,222,225),bold=False): d.text((x*S,y*S),text,font=ImageFont.truetype('DejaVuSans-Bold.ttf' if bold else 'DejaVuSans.ttf',round(size*S)),fill=color,anchor='mm')
for n,x,y in knobs: label(x-6 if n=='algorithm' else x,y-53,n.upper(),9.5 if n=='algorithm' else (10.8 if n=='presets' else 12))
for n,x,y,txt in buttons:
    if n=='play_stop': label(x,y-4,'PLAY',6); label(x,y+4,'STOP',6)
    else: label(x,y,txt,8)
# Both marks live well inside the top flat face (not the bevel).
label(133,34,'FM-1',16,(205,209,214),True)
label(646,34,'FM1-EMU',14,(188,195,206),True)
label(646,47,'VIRTUAL SYNTHESIZER',5.5,(152,160,173))
im.save(P/'panel.jpg',quality=87,subsampling=0)
# Original fine micro-relief, tiled statistically rather than copied from a photograph.
h=rng.normal(0,1,(384,384)); gy,gx=np.gradient(h); normal=np.stack([-gx*.045,-gy*.045,np.ones_like(h)],axis=-1); normal/=np.linalg.norm(normal,axis=-1,keepdims=True)
normal_img=Image.fromarray(np.uint8(np.clip((normal*.5+.5)*255,0,255))); normal_img.save(P/'grain-normal.jpg',quality=75)
atlas=t.visual.material.PBRMaterial(name='original printed graphite',baseColorTexture=Image.open(P/'panel.jpg'),normalTexture=normal_img,roughnessFactor=.58,metallicFactor=.13)
body.baseColorFactor=[.045,.048,.053,1]; body.roughnessFactor=.48; body.metallicFactor=.28
metal.roughnessFactor=.32; rim.roughnessFactor=.38; rim.baseColorFactor=[.095,.10,.11,1]

shape('enclosure',(423,256),722,510,43,-.017,0,9,body,atlas,atlas=True)
shape('keyboard_well',(423,405),678,160,25,.00005,.001,3,recess)
shape('button_well',(643,226),264,105,12,.00005,.0008,2,recess)
shape('lcd_bezel',(350,168),306,306,12,.00005,.002,4,glass)
for name,x,y in knobs:
    shape('mount_'+name,(x,y),38,38,18,.00005,.0015,3,recess)
    # Knurling is true alternating-radius geometry; smooth bevel rings at ends.
    n=64; a=np.arange(n)*2*np.pi/n; v=[]
    for z,r,kn in [(.0015,12,False),(.0025,14,True),(.0053,14,True),(.0065,12,False)]:
        rad=(r+(.65*(np.arange(n)%2) if kn else np.zeros(n)))*s; v.extend(np.c_[rad*np.cos(a),rad*np.sin(a),np.full(n,z)])
    f=[]
    for k in range(3):
        for i in range(n):
            j=(i+1)%n; a0=k*n+i; b0=k*n+j; f.extend([[a0,b0,b0+n],[a0,b0+n,a0+n]])
    node='knob_'+name; mesh(node,v,f,metal,(x,y)); top=np.vstack([np.array(v)[-n:],[[0,0,.0065]]]); mesh(node+'_top',top,[[n,i,(i+1)%n] for i in range(n)],rim,parent=node)
    # Indicator is a separate child rotating with its knob.
    g=t.creation.box(extents=[.0006,.0019,.00015]); g.apply_translation([0,.0015,.00658]); mesh(node+'_indicator',g.vertices,g.faces,ink,parent=node)
for name,x,y,txt in buttons:
    w,h=(43,26) if name.startswith('oct') else (32,32); shape('button_'+name,(x,y),w,h,6,.001,.004,3,rubber,atlas,atlas=True)
# Convert desired sRGB key color to linear glTF base color.
key_rgb=tuple(((v/255+.055)/1.055)**2.4 for v in (42,43,46))
key_side=mat('dark key sides',key_rgb,.48)
# Original legends on replaceable per-key textures. Emissive factor still covers the entire cap.
legends=['OP1','OP2','OP3','OP4','OP5','OP6','PIT','GLO','MONO','POLY','']
def key_texture(text):
    k=Image.new('RGB',(128,256),(42,43,46)); kd=ImageDraw.Draw(k)
    # Soft satin variation, original generated artwork.
    pix=np.array(k,dtype=float); yy,xx=np.mgrid[:256,:128]; sheen=3*np.exp(-((xx-46)/48)**2-((yy-70)/110)**2); pix+=sheen[:,:,None]; k=Image.fromarray(np.uint8(pix)); kd=ImageDraw.Draw(k)
    kd.rounded_rectangle((58,40 if text else 67,70,125 if text else 189),radius=6,fill=(217,220,223))
    if text: kd.text((64,183),text,font=ImageFont.truetype('DejaVuSans.ttf',24 if len(text)<4 else 21),fill=(218,221,225),anchor='mm')
    return k
for name,x,y in keys:
    text=legends[int(name[-2:])] if 'upper' in name else ''
    cap=t.visual.material.PBRMaterial(name=name+'_led',baseColorTexture=key_texture(text),baseColorFactor=[1.,1.,1.,1.],roughnessFactor=.49,metallicFactor=0,emissiveFactor=[0,0,0])
    shape(name,(x,y),32,64,15,.001,.0034,4,key_side,cap,topname=name+'_led',local_uv=True)
a=289*s/2; z=.0021; mesh('lcd',[[-a,-a,z],[a,-a,z],[a,a,z],[-a,a,z]],[[0,1,2],[0,2,3]],mat('lcd_black',(0,0,0),.2),(350,168),uv=np.array([[0,0],[1,0],[1,1],[0,1]]))
scene.export(P/'fm1.glb',include_normals=True); print((P/'fm1.glb').stat().st_size)
