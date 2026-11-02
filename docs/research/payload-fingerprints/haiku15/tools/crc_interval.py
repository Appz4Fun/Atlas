"""Demo: match two uploads of the same file split at different part sizes,
using only per-part CRC32s (yEnc pcrc32 or PAR2 IFSC slice CRCs), via CRC32 combine.
No payload needed beyond what the headers/PAR2 index already carry."""
import os, zlib, math, random
POLY=0xEDB88320
def mulmod(a,b):
    p=0
    while b:
        if b&0x80000000: p^=a
        b=(b<<1)&0xFFFFFFFF
        a=(a>>1)^POLY if a&1 else a>>1
    return p
def x8n(n):  # x^(8n) mod p, reflected
    r=0x80000000  # x^0
    # build x^(2^k) table by squaring x^8
    p=0x00800000  # x^8 in reflected form
    while n:
        if n&1: r=mulmod(r,p)
        p=mulmod(p,p); n>>=1
    return r
def combine(c1,c2,len2): return mulmod(x8n(len2),c1)^c2
def parts(data,sz): return [(i,min(i+sz,len(data)),zlib.crc32(data[i:i+sz])) for i in range(0,len(data),sz)]
def interval_crcs(ps,grid,missing=set()):
    out={}; cur=None; start=None
    for idx,(b,e,c) in enumerate(ps):
        if idx in missing: cur=None; continue
        if b%grid==0: cur=c; start=b
        elif cur is not None: cur=combine(cur,c,e-b)
        if cur is not None and (e%grid==0 or e==ps[-1][1]): out[(start,e)]=cur
        if e%grid==0: cur=None
    return out
data=os.urandom(60_000_000)
a=parts(data,716800); b=parts(data,768000)
g=math.lcm(716800,768000)
ia=interval_crcs(a,g,missing={5,40}); ib=interval_crcs(b,g)
common=[k for k in ia if k in ib and ia[k]==ib[k]]
assert all(ia[k]==zlib.crc32(data[k[0]:k[1]]) for k in ia)
print("grid",g,"intervals A",len(ia),"B",len(ib),"matching",len(common))
other=bytearray(data); other[15_000_000]^=1
ic=interval_crcs(parts(bytes(other),768000),g)
print("one-bit-different file matching intervals:",sum(1 for k in ia if k in ic and ia[k]==ic[k]),"of",len(ia))
