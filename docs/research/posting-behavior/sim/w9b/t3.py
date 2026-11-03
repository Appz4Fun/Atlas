import sys,collections;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
H=[r for r in R if r['fam']=='highwinds']
bz=collections.defaultdict(list)
for r in H: bz[r['nzb']].append(r)
print('| NZB | articles | fx hosts | distinct S | distinct (fx,S) | S constant | N range | N steps | N monotone vs article order | T span s | T-date med s |')
for z,v in bz.items():
    hosts={r['hw'][0] for r in v}; S={r['hw'][1] for r in v}; HS={(r['hw'][0],r['hw'][1]) for r in v}
    # article order: row order within file groups; use row index; also check N vs decoded T
    mono=0;tot=0;steps=collections.Counter()
    for hs in HS:
        w=[r for r in v if (r['hw'][0],r['hw'][1])==hs]
        for a,b in zip(w,w[1:]):
            tot+=1; mono+= b['hw'][2]>a['hw'][2]; steps[b['hw'][2]-a['hw'][2]]+=1
    Ns=[r['hw'][2] for r in v]; T=[r['t'] for r in v]
    d=sorted(r['t']-r['date'] for r in v)
    print('| %s | %d | %s | %d | %d | %s | %d-%d | %s | %d/%d | %.0f | %.0f |'%(z,len(v),','.join(sorted(hosts)),len(S),len(HS),len(S)==1,min(Ns),max(Ns),dict(steps.most_common(3)),mono,tot,max(T)-min(T),d[len(d)//2]))
# N vs T: rank correlation within session
import itertools
for z,v in bz.items():
    w=sorted(v,key=lambda r:r['hw'][2]); inv=sum(b['t']<a['t'] for a,b in zip(w,w[1:]))
    print(z,'sorted by N: T decreases',inv,'of',len(w)-1,'; R field monotone in N?',sum(b['hw'][3]>a['hw'][3] for a,b in zip(w,w[1:])),'/',len(w)-1)
# is row order == file order? compare row order to N order
for z,v in bz.items():
    print(z,'row order N increasing',sum(b['hw'][2]>a['hw'][2] for a,b in zip(v,v[1:])),'/',len(v)-1, 'distinct dates',len({r['date'] for r in v}))
