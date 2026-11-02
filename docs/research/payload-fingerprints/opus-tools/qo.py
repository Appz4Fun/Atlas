import sys,os,zlib,struct
sys.path.insert(0,'/tmp/w4a/bin')
from r5 import parse,vint
for f in sys.argv[1:]:
    b,hs,end,td=parse(f)
    for h in hs:
        if h.get('name')=='QO':
            qoff=h['off']; blob=b[qoff+h['hdrlen']:qoff+h['hdrlen']+h['datasize']]
            p=0
            while p<len(blob):
                crc=struct.unpack('<I',blob[p:p+4])[0]; sz,q=vint(blob,p+4); body=blob[q:q+sz]
                ok=zlib.crc32(blob[p+4:q+sz])&0xffffffff==crc
                fl,r=vint(body,0); off,r=vint(body,r); dsz,r=vint(body,r); data=body[r:r+dsz]
                tgt=qoff-off
                same = b[tgt:tgt+len(data)]==data
                print(os.path.basename(f),'rec crc',ok,'flags',fl,'off',off,'-> abs',tgt,'datalen',dsz,'matches orig hdr bytes',same, data[-25:])
                p=q+sz
