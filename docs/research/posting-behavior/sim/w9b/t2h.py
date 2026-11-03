# characterise overlapping "other" NZB pairs (dedup): fixture-like dates, file counts; and nyuu row-run overlap magnitudes
import sys,collections,itertools;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
ids=collections.defaultdict(set);ds=collections.defaultdict(set);fc=collections.defaultdict(collections.Counter)
for r in R: ids[r['nzb']].add(r['mid']); fc[r['nzb']][r['fam']]+=1; ds[r['nzb']].add(r['date']) if r['date'] else None
seen=set();keep=[]
for z in sorted(ids):
    k=frozenset(ids[z])
    if k not in seen: seen.add(k); keep.append(z)
oz=[z for z in keep if fc[z].most_common(1)[0][0]=='other' and ds[z]]
sp={z:(min(ds[z]),max(ds[z])) for z in oz}
pairs=[(a,b) for a,b in itertools.combinations(oz,2) if sp[a][0]<=sp[b][1] and sp[b][0]<=sp[a][1]]
fx=lambda z: all(d%1000==0 for d in ds[z]) or min(ds[z])<946684800
c=collections.Counter(('fixture-like' if fx(a) or fx(b) else 'plausible', 'both>=3 dates' if len(ds[a])>=3 and len(ds[b])>=3 else 'small') for a,b in pairs)
print('other dedup overlapping pairs',len(pairs),c)
pl=[(a,b) for a,b in pairs if not(fx(a) or fx(b))]
for a,b in pl[:6]: print(a,b,sp[a],sp[b],len(ds[a]),len(ds[b]),len(ids[a]&ids[b]))
print('plausible pairs with >=3 dates each:')
for a,b in pl:
    if len(ds[a])>=3 and len(ds[b])>=3:
        da=sorted(ds[a]); db=sorted(ds[b])
        dm=lambda z: collections.Counter(r['dom'] for r in R if r['nzb']==z).most_common(1)[0][0]
        print(a,b,sp[a],sp[b],len(da),len(db),'shared ids',len(ids[a]&ids[b]),'same date set',da==db,dm(a),dm(b))
