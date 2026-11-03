import sys,glob,hashlib,struct,os
def packets(path):
    b=open(path,'rb').read(); i=0
    while True:
        i=b.find(b'PAR2\0PKT',i)
        if i<0: return
        ln=struct.unpack('<Q',b[i+8:i+16])[0]
        yield b[i:i+ln]; i+=ln
for run in sys.argv[1:]:
    setids=set(); rec={}; creator=None; mainbody=None
    for f in sorted(glob.glob(run+'/*.par2')):
        for p in packets(f):
            setid=p[32:48].hex(); t=p[48:64].rstrip(b'\0'); setids.add(setid)
            if t.endswith(b'RecvSlic'):
                e=struct.unpack('<I',p[64:68])[0]; rec[e]=hashlib.md5(p[68:]).hexdigest()[:12]
            elif t.endswith(b'Creator'): creator=p[64:].rstrip(b'\0').decode()
            elif t.endswith(b'Main'): mainbody=hashlib.md5(p[64:]).hexdigest()[:12]
    print(os.path.basename(run),'setid',sorted(setids),'main',mainbody,'creator',repr(creator))
    print('   recovery exp->md5',dict(sorted(rec.items())))
