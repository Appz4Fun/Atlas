import sys,collections,itertools;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
ids=collections.defaultdict(set); fc=collections.defaultdict(collections.Counter); tt=collections.defaultdict(list)
for r in R:
    ids[r['nzb']].add(r['mid']); fc[r['nzb']][r['fam']]+=1
    t=r['t'] if (r['t'] is not None and r['date'] and abs(r['t']-r['date'])<86400) else r['date']
    if t: tt[r['nzb']].append(t)
fam={z:c.most_common(1)[0][0] for z,c in fc.items()}
# dedupe identical/near-identical id sets
canon={}; groups=collections.defaultdict(list)
for z in sorted(ids):
    key=frozenset(ids[z]); groups[key].append(z)
print('distinct message-id sets',len(groups),'of',len(ids))
reps=[v[0] for v in groups.values()]
for f in ('nyuu-style','highwinds','other'):
    zs=[z for z in reps if fam[z]==f]; sp={z:(min(tt[z]),max(tt[z])) for z in zs if tt[z]}
    pr=[(a,b) for a,b in itertools.combinations(sp,2) if sp[a][0]<=sp[b][1] and sp[b][0]<=sp[a][1]]
    shared=[(a,b) for a,b in pr if ids[a]&ids[b]]
    print(f,'distinct NZBs',len(zs),'overlapping pairs',len(pr),'of which share msg-ids',len(shared))
    for a,b in pr[:8]: print('  ',a,b,round(sp[a][0]),round(sp[a][1]-sp[a][0],1),round(sp[b][0]-sp[a][0],1),round(sp[b][1]-sp[b][0],1),len(ids[a]),len(ids[b]),len(ids[a]&ids[b]))
