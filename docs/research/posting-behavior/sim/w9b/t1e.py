# checks: copy pair with disjoint ids; NZB listing order vs time order of files (nyuu)
import sys,collections;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
for z in ('3f6ae46b7d','6169fd86d5'):
    v=[r for r in R if r['nzb']==z]; print(z,len(v),v[0]['poster'][:30],v[0]['dom'],sorted(r['t'] for r in v)[:3])
a=sorted(r['t'] for r in R if r['nzb']=='3f6ae46b7d'); b=sorted(r['t'] for r in R if r['nzb']=='6169fd86d5'); print('identical ms lists',a==b)
G=collections.OrderedDict()
for r in R:
    if r['fam']=='nyuu-style' and r['date'] and abs(r['t']-r['date'])<86400: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
bz=collections.defaultdict(list)
for k,v in G.items(): bz[k[0]].append((min(x['i'] for x in v),min(x['t'] for x in v)))
same=n=0
for z,f in bz.items():
    if len(f)<2: continue
    n+=1; same+= [x[1] for x in sorted(f)]==sorted(x[1] for x in f)
print('nyuu NZBs whose listing order == time order',same,'/',n)
