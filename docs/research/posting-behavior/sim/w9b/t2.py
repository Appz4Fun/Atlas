import sys,collections,random,itertools;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
def ts(r):
    if r['t'] is not None and r['date'] and abs(r['t']-r['date'])<86400: return r['t']
    return float(r['date']) if r['date'] else None
G=collections.OrderedDict()
for r in R: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
bz=collections.defaultdict(list); famz={}
for k,v in G.items():
    tt=[x for x in map(ts,v) if x is not None]
    if not tt: continue
    bz[k[0]].append(dict(nzb=k[0],start=min(tt),end=max(tt)))
for z in bz:
    famz[z]=collections.Counter(r['fam'] for r in R if r['nzb']==z).most_common(1)[0][0] if False else None
fc=collections.defaultdict(collections.Counter)
for r in R: fc[r['nzb']][r['fam']]+=1
famz={z:c.most_common(1)[0][0] for z,c in fc.items()}
# real overlap of NZB spans
print('| family | NZBs | NZBs with >=2 files | overlapping NZB pairs | NZBs in any overlap |')
for fam in ('nyuu-style','highwinds','ngPost','other'):
    zs=[z for z in bz if famz[z]==fam]; sp={z:(min(f['start'] for f in bz[z]),max(f['end'] for f in bz[z])) for z in zs}
    pairs=[(a,b) for a,b in itertools.combinations(zs,2) if sp[a][0]<=sp[b][1] and sp[b][0]<=sp[a][1]]
    inv=set(itertools.chain(*pairs))
    print('| %s | %d | %d | %d | %d |'%(fam,len(zs),sum(len(bz[z])>1 for z in zs),len(pairs),len(inv)))
    if pairs: print('   pairs:',[(a,b,sp[a],sp[b],len(bz[a]),len(bz[b])) for a,b in pairs[:5]])
def link(files,maxgap):
    # greedy one-to-one end->start matching by smallest nonneg gap
    E=sorted(range(len(files)),key=lambda i:files[i]['end']); S=sorted(range(len(files)),key=lambda i:files[i]['start'])
    cand=[]
    import bisect
    starts=[files[i]['start'] for i in S]
    for i in E:
        e=files[i]['end']; j=bisect.bisect_left(starts,e)
        while j<len(S) and starts[j]-e<=maxgap:
            if S[j]!=i: cand.append((starts[j]-e,i,S[j]))
            j+=1
    cand.sort(); used_o=set(); used_i=set(); par=list(range(len(files)))
    def f(x):
        while par[x]!=x: par[x]=par[par[x]]; x=par[x]
        return x
    for g,i,j in cand:
        if i in used_o or j in used_i or f(i)==f(j): continue
        used_o.add(i); used_i.add(j); par[f(i)]=f(j)
    return [f(i) for i in range(len(files))]
def pr(files,lab):
    tp=fp=fn=0
    for a,b in itertools.combinations(range(len(files)),2):
        same=files[a]['nzb']==files[b]['nzb']; pred=lab[a]==lab[b]
        tp+=same and pred; fp+=pred and not same; fn+=same and not pred
    return tp/(tp+fp) if tp+fp else float('nan'), tp/(tp+fn) if tp+fn else float('nan')
random.seed(1)
idsets=collections.defaultdict(set)
for r in R: idsets[r['nzb']].add(r['mid'])
seen={};reps=set()
for z in sorted(idsets):
    k=frozenset(idsets[z])
    if k not in seen: seen[k]=z; reps.add(z)
# also drop the near-duplicate (same span & file count) copies
spn={}
for z in sorted(r for r in reps if bz.get(r)):
    k=(round(min(f['start'] for f in bz[z]),3),len(bz[z]))
    if k in spn: reps.discard(z)
    else: spn[k]=z
print('deduplicated NZBs',len(reps))
print('\nSimulated overlap (time-shift real NZBs so all start within a window of W seconds)')
print('| family | K NZBs pooled | window W s | maxgap s | trials | precision | recall | chance precision |')
for fam,maxgap in (('nyuu-style',0.5),('nyuu-style',2.0),('nyuu-style',5.0),('other',2.0)):
    zs=[z for z in bz if z in reps and famz[z]==fam and len(bz[z])>=3]
    for K in ((2,5,10,20) if maxgap==2.0 else (5,)):
        for Wn in (0,60):
            P=[];Rc=[];C=[]
            for t in range(100):
                pick=random.sample(zs,K); files=[]
                for z in pick:
                    s0=min(f['start'] for f in bz[z]); off=random.uniform(0,Wn)
                    sh=off-s0
                    if fam=='other': sh=round(sh)  # keep whole-second dates
                    for f in bz[z]: files.append(dict(nzb=z,start=f['start']+sh,end=f['end']+sh))
                lab=link(files,maxgap); p,r=pr(files,lab); P.append(p); Rc.append(r)
                n=collections.Counter(f['nzb'] for f in files); tot=len(files)
                C.append(sum(c*(c-1)/2 for c in n.values())/(tot*(tot-1)/2))
            m=lambda a:sum(x for x in a if x==x)/max(1,sum(1 for x in a if x==x))
            print('| %s | %d | %d | %.0f | 100 | %.3f | %.3f | %.3f |'%(fam,K,Wn,maxgap,m(P),m(Rc),m(C)))
# baseline: no pooling, each NZB alone (recall ceiling)
for fam in ('nyuu-style','other'):
    zs=[z for z in bz if z in reps and famz[z]==fam and len(bz[z])>=2]; Rs=[]
    for z in zs:
        files=[dict(nzb=z,**{k:f[k] for k in('start','end')}) for f in bz[z]]
        Rs.append(pr(files,link(files,2.0))[1])
    print(fam,'single-NZB recall (maxgap 2s) mean',round(sum(Rs)/len(Rs),3),'n',len(zs))
