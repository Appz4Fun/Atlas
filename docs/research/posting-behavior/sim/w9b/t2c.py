# extra: wider windows and candidate ambiguity for nyuu-style simulated pooling
import sys,random,collections,bisect;sys.argv=[sys.argv[0],sys.argv[1]]
exec(open(sys.argv[1]+'/t2.py').read().split("random.seed(1)")[0])
random.seed(2)
idsets=collections.defaultdict(set)
for r in R: idsets[r['nzb']].add(r['mid'])
seen=set();reps=[]
for z in sorted(idsets):
    k=frozenset(idsets[z])
    if k not in seen and famz[z]=='nyuu-style' and len(bz[z])>=3: seen.add(k); reps.append(z)
spn=set();zs=[]
for z in reps:
    k=(round(min(f['start'] for f in bz[z]),3),len(bz[z]))
    if k not in spn: spn.add(k); zs.append(z)
print('nyuu NZBs usable',len(zs))
print('| K | W s | precision | recall | true successor is nearest start (top-1) | mean candidates within 2 s |')
for K in (5,10):
    for Wn in (0,60,600,3600):
        P=[];Rc=[];top=[0,0];cands=[]
        for t in range(100):
            pick=random.sample(zs,K); files=[]
            for z in pick:
                s0=min(f['start'] for f in bz[z]); sh=random.uniform(0,Wn)-s0
                for f in sorted(bz[z],key=lambda f:f['start']): files.append(dict(nzb=z,start=f['start']+sh,end=f['end']+sh))
            lab=link(files,2.0); p,r=pr(files,lab); P.append(p); Rc.append(r)
            st=sorted((f['start'],i) for i,f in enumerate(files)); ss=[x[0] for x in st]
            for i,f in enumerate(files):
                own=sorted([g for g in files if g['nzb']==f['nzb'] and g['start']>=f['start'] and g is not f],key=lambda g:g['start'])
                if not own: continue
                j=bisect.bisect_left(ss,f['end']); c=[files[k] for s,k in st[j:] if s-f['end']<=2.0 and files[k] is not f]
                cands.append(len(c))
                if c: top[1]+=1; top[0]+= c[0]['nzb']==f['nzb']
        m=lambda a:sum(x for x in a if x==x)/max(1,sum(1 for x in a if x==x))
        print('| %d | %d | %.3f | %.3f | %.3f | %.2f |'%(K,Wn,m(P),m(Rc),top[0]/max(1,top[1]),sum(cands)/len(cands)))
