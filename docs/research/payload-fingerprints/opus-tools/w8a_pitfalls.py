# w8a: pitfalls - tsx (sub-second mtime) mismatch rate, ngPost auto-V, two-file combination
import sys, random, math
sys.path.insert(0,'docs/research/payload-fingerprints/opus-tools')
from w7a_hp_model import pred, H
r=random.Random(2)
for U in (500*2**20, 4*2**30):
    d=sum(H(L,0,U,0)!=H(L,0,U,7) for L in range(20,91))/71
    print(f'U={U}: fraction of L 20-90 where sub-second mtime changes H: {d:.2f}')
# ngPost auto volume: vol=floor(postMB/99)+1 when size/vol>99 -> V is f(U): same file -> same V
def ngV(U,base=42):
    mb=U/2**20
    return (int(mb//99)+1 if mb/base>99 else base)*2**20  # approximation of ngPost rule
print('ngPost V for 3GB, 8GB, 20GB:',[ngV(x*2**30)//2**20 for x in (3,8,20)])
# two independent files per post: per-pair p multiplies
p1=5.28e-9
for k in (1,2,3):
    p=p1**k; print(f'{k} file(s): p={p:.1e} false pairs @1e7={p*5e13:.2e} @1e8={p*5e15:.2e}')
