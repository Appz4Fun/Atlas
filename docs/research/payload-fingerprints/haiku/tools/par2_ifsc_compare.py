import sys,glob,hashlib,struct
def packets(path):
    b=open(path,'rb').read(); i=0
    while (i:=b.find(b'PAR2\0PKT',i))>=0:
        ln=struct.unpack('<Q',b[i+8:i+16])[0]; yield b[i:i+ln]; i+=ln
for run in sys.argv[1:]:
    names={}; ifsc={}
    for f in glob.glob(run+'/*.par2'):
        for p in packets(f):
            t=p[48:64].rstrip(b'\0')
            if t.endswith(b'FileDesc'):
                fid=p[64:80]; ln=struct.unpack('<Q',p[112:120])[0]; names[fid]=(p[120:].rstrip(b'\0').decode(),ln,p[80:96].hex()[:8])
            elif t.endswith(b'IFSC'):
                ifsc[p[64:80]]=hashlib.md5(p[80:]).hexdigest()[:12]
    print(run, sorted((names[k][0],names[k][1],'fullmd5',names[k][2],'ifsc-list',ifsc.get(k)) for k in names))
