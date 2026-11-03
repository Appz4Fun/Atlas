import sys,os,hashlib,struct,zlib
sys.path.insert(0,'/tmp/w4a/bin')
import r5
from r5 import vint,aesdec
f,pw=sys.argv[1],sys.argv[2]
b=open(f,'rb').read()
# re-walk to get decrypted QO header bytes: monkeypatch by re-parsing with capture
orig=r5.aesdec; cap={}
b2,hs,end,td=r5.parse(f,pw)
qo=[h for h in hs if h.get('name')=='QO'][0]
# decrypt header to get extra area
kdf=[h for h in hs if h['type']==4][0]
salt=b[8+4+1+1+1+1+1+1:8+4+6+16]
# header salt: enc header layout crc4 size1 type1 flags1 ver1 encflags1 kdf1 salt16
salt=b[8+10:8+26]; key=hashlib.pbkdf2_hmac('sha256',pw.encode(),salt,1<<kdf['kdf'],32)
p=qo['off']; iv=b[p:p+16]; h=aesdec(key,iv,b[p+16:p+qo['hdrlen']])
hs_,q=vint(h,4); hdr=h[4:q+hs_]
q2=0; _,q2=vint(hdr,0); t,q2=vint(hdr,q2); fl,q2=vint(hdr,q2); ex,q2=vint(hdr,q2); ds,q2=vint(hdr,q2)
e=hdr[len(hdr)-ex:]; r=0
while r<len(e):
    sz,r2=vint(e,r); t,r3=vint(e,r2)
    if t==1:
        v,r4=vint(e,r3); cf,r4=vint(e,r4); kc=e[r4]; s=e[r4+1:r4+17]; fiv=e[r4+17:r4+33]
        k2=hashlib.pbkdf2_hmac('sha256',pw.encode(),s,1<<kc,32)
        dp=p+qo['hdrlen']; data=aesdec(k2,fiv,b[dp:dp+ds])
    r=r2+sz
P=0;n=0
while P<len(data)-4:
    crc=struct.unpack('<I',data[P:P+4])[0]; sz,Q=vint(data,P+4)
    if sz==0: break
    ok=zlib.crc32(data[P+4:Q+sz])&0xffffffff==crc
    if not ok: break
    body=data[Q:Q+sz]; fl,R=vint(body,0); off,R=vint(body,R); dsz,R=vint(body,R); n+=1
    print('rec',n,'crcok',ok,'off',off,'abs',p-off,'cached hdr len',dsz,body[R:R+dsz][-16:])
    P=Q+sz
