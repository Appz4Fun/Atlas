import hashlib, struct, zlib, sys
def pkt(setid, typ, body):
    rest = setid + typ + body
    return b'PAR2\0PKT' + struct.pack('<Q', 64 + len(body)) + hashlib.md5(rest).digest() + rest
files = sys.argv[2:]; slice_ = 384000
fds=[]; ifscs=[]; ids=[]
for f in files:
    d=open(f,'rb').read(); name=f.encode(); name += b'\0'*((-len(name))%4)
    h16=hashlib.md5(d[:16384]).digest()
    fid=hashlib.md5(h16+struct.pack('<Q',len(d))+f.encode()).digest(); ids.append(fid)
    fds.append((fid, fid+hashlib.md5(d).digest()+h16+struct.pack('<Q',len(d))+name))
    sl=b''
    for i in range(0,len(d),slice_):
        s=d[i:i+slice_].ljust(slice_,b'\0'); sl+=hashlib.md5(s).digest()+struct.pack('<I',zlib.crc32(s))
    ifscs.append((fid, fid+sl))
ids.sort(); main=struct.pack('<QI',slice_,len(ids))+b''.join(ids)
setid=hashlib.md5(main).digest()
out=pkt(setid,b'PAR 2.0\0Main\0\0\0\0',main)
for (fid,b),(_,ib) in zip(fds,ifscs): out+=pkt(setid,b'PAR 2.0\0FileDesc',b)+pkt(setid,b'PAR 2.0\0IFSC\0\0\0\0',ib)
out+=pkt(setid,b'PAR 2.0\0Creator\0',b'w10b-pygen\0\0')
# a fake vol: 2 recovery slices of random-ish bytes interleaved with critical packets, ~ 1 MB
vol=b''
for e in range(2): vol+=pkt(setid,b'PAR 2.0\0RecvSlic',struct.pack('<I',e)+bytes(slice_)) + out
open(sys.argv[1],'wb').write(out); open(sys.argv[1].replace('.par2','.vol0+2.par2'),'wb').write(vol)
print('setid',setid.hex())
