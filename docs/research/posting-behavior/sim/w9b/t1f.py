# test-1 gaps for "other" family per ID domain, excluding fixture-like NZBs (all dates %1000==0)
import sys,collections;sys.path.insert(0,sys.argv[1])
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
    if r['fam']=='other' and r['nzb'] in keep and r['date']: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
bz=collections.defaultdict(list); dom={}
for k,v in G.items(): bz[k[0]].append(k[1]); dom[k[0]]=collections.Counter(r['dom'] for r in v).most_common(1)[0][0]
def q(a,p): a=sorted(a); return a[min(len(a)-1,int(p*(len(a)-1)+.5))]
agg=collections.defaultdict(list); nz=collections.Counter()
for z,d in bz.items():
    if all(x%1000==0 for x in d) or len(d)<2: continue
    d=sorted(d); nz[dom[z]]+=1; agg[dom[z]]+= [b-a for a,b in zip(d,d[1:])]
print('| domain | NZBs | gaps | median s | p90 s | frac<2s | frac==0 |')
for k,g in sorted(agg.items(),key=lambda x:-len(x[1])):
    print('| %s | %d | %d | %d | %d | %.2f | %.2f |'%(k or '(none)',nz[k],len(g),q(g,.5),q(g,.9),sum(x<2 for x in g)/len(g),sum(x==0 for x in g)/len(g)))
