import sys,collections,statistics;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
def ts(r):
    if r['t'] is not None and r['date'] and abs(r['t']-r['date'])<86400: return r['t'],True
    return (float(r['date']),False) if r['date'] else (None,False)
G=collections.OrderedDict()
for r in R: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
runs=len(files(R))
print('file groups (nzb,date,poster)',len(G),'row-runs',runs)
bz=collections.defaultdict(list)
for k,v in G.items():
    tt=[ts(r) for r in v]; tt=[x for x in tt if x[0] is not None]
    if not tt: continue
    fam=collections.Counter(r['fam'] for r in v).most_common(1)[0][0]
    dec=all(x[1] for x in tt)
    bz[k[0]].append(dict(start=min(x[0] for x in tt),end=max(x[0] for x in tt),n=len(v),fam=fam,dec=dec,date=k[1]))
def q(a,p): a=sorted(a); return a[min(len(a)-1,int(p*(len(a)-1)+.5))]
stats=collections.defaultdict(list); ov=collections.Counter(); ovs=collections.defaultdict(list); zst=collections.Counter()
gapsD=collections.defaultdict(list)
for z,fs in bz.items():
    fam=collections.Counter(f['fam'] for f in fs).most_common(1)[0][0]
    fs.sort(key=lambda f:(f['start'],f['end']))
    zst[fam]+=1
    for a,b in zip(fs,fs[1:]):
        g=b['start']-a['end']
        key=fam+(' (decoded)' if a['dec'] and b['dec'] else ' (NZB date)')
        stats[key].append(g)
        gapsD[fam].append(b['date']-a['date'])
        if g<0: ov[key]+=1; ovs[key].append(g)
print('nzbs per fam',zst)
print('| family / timestamp source | gaps | median s | p90 s | frac <2s | frac <0 (overlap) | frac ==0 |')
for k,g in sorted(stats.items()):
    print('| %s | %d | %.3f | %.3f | %.3f | %.3f | %.3f |'%(k,len(g),q(g,.5),q(g,.9),sum(x<2 for x in g)/len(g),sum(x<0 for x in g)/len(g),sum(x==0 for x in g)/len(g)))
print('overlap magnitudes median',{k:round(q(v,.5),3) for k,v in ovs.items()})
print('NZB-date-only gaps (consecutive file dates, all families):')
for k,g in gapsD.items(): print(k,len(g),'med',q(g,.5),'p90',q(g,.9),'<2s',round(sum(0<=x<2 for x in g)/len(g),3),'==0',round(sum(x==0 for x in g)/len(g),3))
# per-NZB: fraction of NZBs with any overlap (decoded only)
for fam in ('nyuu-style','highwinds'):
    n=a=0
    for z,fs in bz.items():
        if collections.Counter(f['fam'] for f in fs).most_common(1)[0][0]!=fam or len(fs)<2: continue
        n+=1; a+=any(b['start']<x['end'] for x,b in zip(fs,fs[1:]))
    print(fam,'NZBs with >=2 files',n,'with any overlap',a)
# durations
for fam in ('nyuu-style','highwinds'):
    d=[f['end']-f['start'] for fs in bz.values() for f in fs if f['fam']==fam and f['n']>1]
    print(fam,'file duration med',round(q(d,.5),3),'p90',round(q(d,.9),3))
