import sys,collections;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
c=collections.Counter(r['fam'] for r in R);print(c)
# nzb-level family = majority
bz=collections.defaultdict(list)
for r in R: bz[r['nzb']].append(r)
fc=collections.Counter()
for z,v in bz.items():
    fc[collections.Counter(r['fam'] for r in v).most_common(1)[0][0]]+=1
print('nzbs',len(bz),fc)
# highwinds decode check
for r in R:
    if r['fam']=='highwinds': print(r['date'],r['t'],r['t']-r['date'],r['mid']);break
d=[r['t']-r['date'] for r in R if r['t'] is not None and r['date']]
d.sort();print('t-date quantiles',[d[int(q*(len(d)-1))] for q in (0,.01,.1,.5,.9,.99,1)])
for fam in ('nyuu-style','highwinds'):
    d=sorted(r['t']-r['date'] for r in R if r['fam']==fam and r['date']);print(fam,[round(d[int(q*(len(d)-1))],3) for q in (0,.01,.1,.5,.9,.99,1)])
# within NZB, is row order monotone in t?
inv=tot=0
for z,v in bz.items():
    ts=[r['t'] for r in v if r['t'] is not None]
    for a,b in zip(ts,ts[1:]):
        tot+=1; inv+= b<a
print('adjacent inversions',inv,tot)
F=files(R);print('files',len(F)); print('segs/file',collections.Counter(min(len(f['rows']),10) for f in F))
# date monotone within NZB in row order?
inv=tot=0
for z,v in bz.items():
    for a,b in zip(v,v[1:]):
        if a['date'] and b['date']: tot+=1; inv+=b['date']<a['date']
print('date inversions',inv,tot)
