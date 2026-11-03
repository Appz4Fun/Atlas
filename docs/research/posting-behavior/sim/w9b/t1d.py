# NZB-level bootstrap CI for nyuu decoded gap median/p90/frac<2s (dedup by id set)
import sys,collections,random;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
ids=collections.defaultdict(set)
for r in R: ids[r['nzb']].add(r['mid'])
seen=set();keep=set()
for z in sorted(ids):
    k=frozenset(ids[z])
    if k not in seen: seen.add(k); keep.add(z)
G=collections.OrderedDict()
for r in R:
    if r['nzb'] in keep and r['fam']=='nyuu-style' and r['date'] and abs(r['t']-r['date'])<86400: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r['t'])
bz=collections.defaultdict(list)
for k,v in G.items(): bz[k[0]].append((min(v),max(v)))
gz={z:[b[0]-a[1] for a,b in zip(sorted(f),sorted(f)[1:])] for z,f in bz.items()}
gz={z:g for z,g in gz.items() if g}
def q(a,p): a=sorted(a); return a[min(len(a)-1,int(p*(len(a)-1)+.5))]
random.seed(3);Z=list(gz);M=[];P=[];F=[]
for b in range(2000):
    g=[x for z in random.choices(Z,k=len(Z)) for x in gz[z]]
    M.append(q(g,.5));P.append(q(g,.9));F.append(sum(x<2 for x in g)/len(g))
ci=lambda a:(round(q(a,.025),3),round(q(a,.975),3))
print('NZBs',len(Z),'gaps',sum(map(len,gz.values())),'median CI',ci(M),'p90 CI',ci(P),'frac<2s CI',ci(F))
# per-NZB median gap distribution
pm=sorted(q(g,.5) for g in gz.values()); print('per-NZB median gap: p10/p50/p90',q(pm,.1),q(pm,.5),q(pm,.9),'max',pm[-1])
