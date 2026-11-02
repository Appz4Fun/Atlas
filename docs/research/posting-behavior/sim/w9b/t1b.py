# robustness for test 1: dedup NZBs, alternative file definition (row runs), within-file inter-segment gaps
import sys,collections;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
ids=collections.defaultdict(set)
for r in R: ids[r['nzb']].add(r['mid'])
seen=set();keep=set()
for z in sorted(ids):
    k=frozenset(ids[z])
    if k not in seen: seen.add(k); keep.add(z)
R=[r for r in R if r['nzb'] in keep]
def q(a,p): a=sorted(a); return a[min(len(a)-1,int(p*(len(a)-1)+.5))]
def ok(r): return r['t'] is not None and r['date'] and abs(r['t']-r['date'])<86400
fc=collections.defaultdict(collections.Counter)
for r in R: fc[r['nzb']][r['fam']]+=1
famz={z:c.most_common(1)[0][0] for z,c in fc.items()}
for defn in ('group(nzb,date,poster)','row-run'):
    if defn=='row-run': F=[f['rows'] for f in files(R)]
    else:
        G=collections.OrderedDict()
        for r in R: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
        F=list(G.values())
    bz=collections.defaultdict(list)
    for v in F:
        if all(ok(r) for r in v): bz[v[0]['nzb']].append((min(r['t'] for r in v),max(r['t'] for r in v),v))
    for fam in ('nyuu-style','highwinds'):
        g=[];seg=[];olap=0
        for z,fs in bz.items():
            if famz[z]!=fam: continue
            fs.sort(key=lambda x:x[:2])
            for a,b in zip(fs,fs[1:]): g.append(b[0]-a[1])
            for s,e,v in fs:
                t=sorted(r['t'] for r in v); seg+= [y-x for x,y in zip(t,t[1:])]
        if g: print('| %s | %s | %d | %.3f | %.3f | %.3f | %.3f | %d | %.3f | %.3f |'%(defn,fam,len(g),q(g,.5),q(g,.9),sum(x<2 for x in g)/len(g),sum(x<0 for x in g)/len(g),len(seg),q(seg,.5) if seg else 0,q(seg,.9) if seg else 0))
print('kept NZBs',len(keep))
# parallel-file signature inside nyuu: interleaving of ms between consecutive date-groups
