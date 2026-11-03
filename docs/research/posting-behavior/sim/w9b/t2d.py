# proxy for the missing size feature: segment count per file. Cost = gap + penalty if counts differ.
import sys,random,collections,bisect,itertools;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
G=collections.OrderedDict()
for r in R:
    if r['fam']=='nyuu-style' and r['date'] and abs(r['t']-r['date'])<86400: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
ids=collections.defaultdict(set)
for r in R: ids[r['nzb']].add(r['mid'])
bz=collections.defaultdict(list)
for k,v in G.items(): bz[k[0]].append(dict(start=min(x['t'] for x in v),end=max(x['t'] for x in v),n=len(v)))
seen=set();zs=[]
for z in sorted(bz):
    key=(round(min(f['start'] for f in bz[z]),3),len(bz[z]))
    if key in seen or len(bz[z])<3: continue
    seen.add(key); zs.append(z)
# how often consecutive files in one NZB share segment count
same=tot=0
for z in zs:
    f=sorted(bz[z],key=lambda x:x['start'])
    for a,b in zip(f,f[1:]): tot+=1; same+=a['n']==b['n']
print('nyuu NZBs',len(zs),'consecutive files with equal segment count',same,'/',tot)
def link(files,maxgap,pen):
    cand=[]
    for i,a in enumerate(files):
        for j,b in enumerate(files):
            g=b['start']-a['end']
            if i!=j and 0<=g<=maxgap: cand.append((g+(pen if a['n']!=b['n'] else 0),i,j))
    cand.sort(); uo=set(); ui=set(); par=list(range(len(files)))
    def f(x):
        while par[x]!=x: par[x]=par[par[x]]; x=par[x]
        return x
    for c,i,j in cand:
        if i in uo or j in ui or f(i)==f(j): continue
        uo.add(i); ui.add(j); par[f(i)]=f(j)
    return [f(i) for i in range(len(files))]
def pr(files,lab):
    tp=fp=fn=0
    for a,b in itertools.combinations(range(len(files)),2):
        s=files[a]['z']==files[b]['z']; p=lab[a]==lab[b]; tp+=s and p; fp+=p and not s; fn+=s and not p
    return tp/(tp+fp) if tp+fp else float('nan'), tp/(tp+fn)
print('| K | W s | penalty s | precision | recall |')
for K,Wn in ((5,0),(10,0),(10,60),(20,0)):
    for pen in (0,1.0):
        random.seed(K*100+Wn); P=[];Rc=[]
        for t in range(100):
            files=[]
            for z in random.sample(zs,K):
                sh=random.uniform(0,Wn)-min(f['start'] for f in bz[z])
                for f in bz[z]: files.append(dict(z=z,start=f['start']+sh,end=f['end']+sh,n=f['n']))
            p,r=pr(files,link(files,2.0,pen)); P.append(p); Rc.append(r)
        m=lambda a:sum(x for x in a if x==x)/max(1,sum(1 for x in a if x==x))
        print('| %d | %d | %.1f | %.3f | %.3f |'%(K,Wn,pen,m(P),m(Rc)))
