import sys,hashlib,struct,zlib
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
def kdf(pw,salt):
    seed=pw.encode('utf-16le')+salt; h=hashlib.sha1(); iv=b''
    for i in range(0x40000):
        h.update(seed+struct.pack('<I',i)[:3])
        if i%0x4000==0: iv+=bytes([h.copy().digest()[19]])
    d=h.digest(); key=b''.join(struct.pack('<I',x) for x in struct.unpack('>4I',d[:16]))
    return key,iv
f=sys.argv[1]; b=open(f,'rb').read(); p=20; pw=sys.argv[2] if len(sys.argv)>2 else 'password'
while p<len(b):
    salt=b[p:p+8]; key,iv=kdf(pw,salt)
    dec=Cipher(algorithms.AES(key),modes.CBC(iv)).decryptor()
    first=dec.update(b[p+8:p+24])
    crc,typ,fl,hs=struct.unpack('<HBHH',first[:7])
    n=(hs+15)//16*16; rest=dec.update(b[p+24:p+8+n]); h=(first+rest)[:hs]
    add=struct.unpack('<I',h[7:11])[0] if (fl&0x8000) else 0
    ok=(zlib.crc32(h[2:])&0xffff)==crc
    print('off',p,'type',hex(typ),'flags',hex(fl),'hsize',hs,'padded',n,'crcok',ok,'data',add)
    p+=8+n+add
    if typ==0x7b: break
print('end',p,'size',len(b))
