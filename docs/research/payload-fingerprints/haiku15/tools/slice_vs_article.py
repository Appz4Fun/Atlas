"""Place a file onto a PAR2 FileDesc entry with zero payload bytes.
Article side: yEnc pcrc32 per article. PAR2 side: IFSC per-slice CRC32 (last slice zero padded).
Both fold to the whole-file CRC32; they also agree at every shared LCM boundary."""
import os, zlib, math
POLY=0xEDB88320; ORDER=2**32-1   # CRC-32 poly is primitive-ish enough here: x has order dividing 2^32-1
def mult(a,b):                    # zlib multmodp, reflected
    m=1<<31; p=0
    while True:
        if a&m:
            p^=b
            if a&(m-1)==0: break
        m>>=1
        if m==0: break
        b=(b>>1)^POLY if b&1 else b>>1
    return p
def xpow(e):                      # x^e mod P (reflected); x^0 = 1<<31, x^1 = 1<<30
    r=1<<31; base=1<<30
    while e:
        if e&1: r=mult(r,base)
        base=mult(base,base); e>>=1
    return r
def combine(c1,c2,len2): return mult(xpow(8*len2),c1)^c2
def unzero(c,k):                  # crc(A||0^k) -> crc(A)
    return mult(xpow((-8*k)%ORDER), c^zlib.crc32(b"\0"*k)) if k else c
def fold(items):
    c=0
    for cc,l in items: c=combine(c,cc,l)
    return c
data=os.urandom(5_000_003)
ART=716800; SL=393216
arts=[(zlib.crc32(data[i:i+ART]),len(data[i:i+ART])) for i in range(0,len(data),ART)]
sl=[]
for i in range(0,len(data),SL):
    s=data[i:i+SL]; sl.append((zlib.crc32(s+b"\0"*(SL-len(s))),len(s)))
last=(unzero(sl[-1][0],SL-sl[-1][1]),sl[-1][1])
fa=fold(arts); fp=fold(sl[:-1]+[last])
print("real",hex(zlib.crc32(data)),"from articles",hex(fa),"from PAR2 IFSC",hex(fp),"match",fa==fp==zlib.crc32(data))
L=ART*SL//math.gcd(ART,SL)
na=L//ART; ns=L//SL; ok=0; n=len(data)//L
for k in range(1,n+1):
    ok+= fold(arts[(k-1)*na:k*na])==fold(sl[(k-1)*ns:k*ns])
print("LCM",L,"interior intervals",n,"matching",ok)
