import random,sys
sys.path.insert(0,'docs/research/yenc-header-forensics/opus/nyuu-id-solver')
from stream_link import cstep,cback,M64
random.seed(5)
st=(random.getrandbits(64),random.getrandbits(64))
x=st
for _ in range(1000): x=cstep(*x)
for _ in range(1000): x=cback(*x)
assert x==st
cache=[]
def rnd():
    global st
    if not cache:
        g=[]
        for _ in range(64): st=cstep(*st); g.append(st[0])
        cache.extend(g)
    return cache.pop()
def mid(): return ''.join(chr((65 if j%2==0 else 97)+(((rnd()>>12)*26)>>52)) for j in range(24))
ms=1700000000000
def nzb(fn,nfiles,nseg):
    global ms
    out=['<nzb>']
    for f in range(nfiles):
        out.append('<file poster="x" subject="f%d"><segments>'%f)
        for s in range(nseg):
            mid(); 
            if random.random()<0.05: mid()
            out.append('<segment bytes="1" number="%d">%s-%d@nyuu</segment>'%(s+1,mid(),ms)); ms+=7
        out.append('</segments></file>')
    out.append('</nzb>'); open(fn,'w').write('\n'.join(out))
for _ in range(37): rnd()
nzb('A.nzb',3,40)
for _ in range(500000): rnd()     # unrelated work / other uploads in same process
nzb('B.nzb',2,30)
st=(random.getrandbits(64),random.getrandbits(64)); cache=[]
nzb('C.nzb',2,30)
