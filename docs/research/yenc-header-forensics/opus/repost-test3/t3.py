# Corpus tests: (1) n-1 tolerance, (2) sd floor 40 vs 70, (3) toverdoos 5-article-size byte-range key, (4) From-less upload regrouping.
# Usage: python -I t3.py <nzb dir>
import sys,re,glob,gzip,hashlib,collections,math,random,statistics as st,xml.etree.ElementTree as ET
here=__file__.rsplit('/',1)[0]
src=open(here+'/../repost-test2/corpus.py').read().split("print(len(paths),'nzbs'")[0]; exec(src)
sys.path.insert(0,here); from synth_pred import predict_vec
H=lambda v: hashlib.sha1(repr(v).encode()).hexdigest()[:16]
posts={}
for r in rows: posts.setdefault(frozenset(r['ids']),r)
P=[r for r in posts.values() if r['w']!='flat' and len(r['b'])>=2]
for r in P:
    f=r['b'][:-1]; r['mean']=st.mean(f); r['sd']=st.pstdev(f) if len(f)>1 else None
hi=collections.defaultdict(list)
for r in P:
    if r['sd'] and 60<=r['sd']<=200: hi[round(r['mean'],-4)].append(r['mean'])
ref={k:st.median(v) for k,v in hi.items() if len(v)>=5}
def gate(r,floor):
    m=ref.get(round(r['mean'],-4))
    if m is None or abs(r['mean']-m)>=1500: return False
    return r['sd'] is None or floor<=r['sd']<=600
def truth(a,b):  # same file content? same filename, or same volume index inside the obfuscated repost family
    if a['fn']==b['fn']: return True
    fam=lambda x:'obfuscated-rar' in x['nzb'].lower() or 'obfuscated_rar' in x['nzb'].lower()
    return fam(a) and fam(b)
print('#### TASK 1+2: exact vs n-1 (and n-2) full-vector matches, gated, floor 40 vs 70')
for floor in (40,70):
    G=list({H(r['b']):r for r in P if gate(r,floor)}.values())   # distinct vectors
    allG=[r for r in P if gate(r,floor)]
    ex=collections.defaultdict(list)
    for r in allG: ex[H(r['b'])].append(r)
    ext=sum(1 for v in ex.values() if len(v)>1 and all(truth(v[0],x) for x in v[1:]))
    exf=sum(1 for v in ex.values() if len(v)>1 and not all(truth(v[0],x) for x in v[1:]))
    bys=collections.defaultdict(list)
    for r in G: bys[len(r['b'])].append(r)
    tol={1:[0,0],2:[0,0]}; ex_lst=[]
    for n,v in bys.items():
        for i in range(len(v)):
            for j in range(i+1,len(v)):
                a,b=v[i],v[j]; mis=sum(x!=y for x,y in zip(a['b'],b['b']))
                if mis in (1,2):
                    t=truth(a,b); tol[mis][0 if t else 1]+=1
                    ex_lst.append((mis,n,t,a['fn'][:28],b['fn'][:28],round(a['sd'] or -1),round(b['sd'] or -1)))
    lost=[r for r in P if gate(r,40) and not gate(r,70)]
    print(f'floor {floor}: gated files {len(allG)}, distinct vectors {len(G)}; exact groups true {ext} false {exf}; '
          f'n-1 extra pairs true {tol[1][0]} false {tol[1][1]}; n-2 extra pairs true {tol[2][0]} false {tol[2][1]}')
    for e in ex_lst: print('   mis',e)
    segh=collections.Counter(min(len(r['b']),20) for r in G); print('   gated distinct vectors by segcount (20=20+):',sorted(segh.items()))
lost=[r for r in P if gate(r,40) and not gate(r,70)]
print('files in sd [40,70):',len(lost),collections.Counter(r['nzb'][15:60] for r in lost).most_common(6))
twinset=set()
for v in collections.defaultdict(list,{}).values(): pass
ex=collections.defaultdict(list)
for r in P: ex[H(r['b'])].append(r)
lt=[r for r in lost if len(ex[H(r['b'])])>1]
print('  of which belong to an exact multi-post group:',len(lt),[(r['fn'][:30],round(r['sd'])) for r in lt][:8])

print('\n#### TASK 3: toverdoos blabla.bin at 5 article sizes (byte-range prediction)')
tv=[r for r in rows if 'toverdoos' in r['nzb'] and r['fn']=='blabla.bin']
arts={'10111222':10111222,'5111222':5111222,'4000111':4000111,'2000111':2000111,'no_article':716800}
V={}
for r in tv:
    for k,a in arts.items():
        if k in r['nzb']: V[a]=(r['b'],r['size'])
print({a:(len(b),s) for a,(b,s) in V.items()})
# non-twin controls: other 100MB-ish Nyuu files at 716800 with the same size field
ctrl=[r for r in P if r['size']==104857600 and len(r['b'])==147 and r['fn']!='blabla.bin']
print('controls at 147 segs, size 104857600:',len(ctrl),[c['fn'][:25] for c in ctrl][:6])
def sc(pred,act):
    r=[a-p for a,p in zip(act,pred)]; m=st.median(r)
    return round(math.sqrt(sum(x*x for x in r)/len(r)),1), round(math.sqrt(sum((x-m)**2 for x in r)/len(r)),1), round(m)
for A in sorted(V):
    for B in sorted(V):
        if A==B: continue
        vA,n=V[A]; p=predict_vec(vA,A,n,B)
        line=f'A={A:>9} -> B={B:>9}: twin {sc(p,V[B][0])}'
        if B==716800: line+=' | controls '+str([sc(p,c['b']) for c in ctrl[:5]])
        print(line)
low=[r['sd'] for r in tv if r['sd']]; print('toverdoos within-file sd:',[round(x,1) for x in low])

print('\n#### TASK 4: regroup files into uploads with From/subject stripped')
# one row per distinct post; truth = source NZB; drop rewritten duplicates (same date+vector seen in another NZB)
seen=set(); F=[]
for r in posts.values():
    if not r['date']: continue
    k=(r['date'],H(r['b']))
    if k in seen: continue
    seen.add(k); F.append(dict(t=int(r['date']),b=r['b'],nzb=r['nzb'],n=len(r['b']),mean=st.mean(r['b'][:-1]) if len(r['b'])>1 else r['b'][0]))
print('files',len(F),'uploads',len({f['nzb'] for f in F}))
def prf(F,lab):
    tp=fp=fn=0
    byL=collections.Counter(lab[i] for i in range(len(F))); byT=collections.Counter(f['nzb'] for f in F)
    both=collections.Counter((lab[i],F[i]['nzb']) for i in range(len(F)))
    tp=sum(c*(c-1)//2 for c in both.values()); pl=sum(c*(c-1)//2 for c in byL.values()); pt=sum(c*(c-1)//2 for c in byT.values())
    return round(tp/max(pl,1),4), round(tp/max(pt,1),4)
def cluster_time(F,T):
    idx=sorted(range(len(F)),key=lambda i:F[i]['t']); lab=[0]*len(F); g=0; last=None
    for i in idx:
        if last is not None and F[i]['t']-last>T: g+=1
        lab[i]=g; last=F[i]['t']
    return lab
def bucket(f): return round(f['mean']/2000) if f['n']>1 else None
def cluster_shape(F,T):
    idx=sorted(range(len(F)),key=lambda i:F[i]['t']); lab=[0]*len(F); groups=[]
    for i in idx:
        f=F[i]; best=None
        for gi,gr in enumerate(groups):
            dt=f['t']-gr['last']
            if dt>T: continue
            ok=True
            if bucket(f) is not None and gr['bucket'] is not None and bucket(f)!=gr['bucket']: ok=False   # article size class
            if ok and gr['vol'] and f['n']>gr['vol']: ok=False         # bigger than the set's volume: different upload
            if ok and gr['closed'] and f['n']==gr['vol']: ok=False     # a short last volume already closed the data run
            if ok and (best is None or dt<best[0]): best=(dt,gi)
        if best is None:
            groups.append(dict(last=f['t'],bucket=bucket(f),vol=f['n'] if f['n']>2 else 0,closed=False)); lab[i]=len(groups)-1
        else:
            gr=groups[best[1]]; lab[i]=best[1]; gr['last']=f['t']
            if gr['bucket'] is None: gr['bucket']=bucket(f)
            if f['n']>gr['vol']: gr['vol']=f['n']
            elif 2<f['n']<gr['vol'] and f['n']>gr['vol']//3: gr['closed']=True
    return lab
for T in (30,120,600):
    print(f' real timeline, time-gap only T={T}s: precision/recall',prf(F,cluster_time(F,T)),' time+shape',prf(F,cluster_shape(F,T)))
random.seed(3)
for W in (3600,600):
    ups=collections.defaultdict(list)
    for f in F: ups[f['nzb']].append(f)
    G=[]
    for u,fs in ups.items():
        t0=min(f['t'] for f in fs); off=random.randint(0,W)
        for f in fs: G.append(dict(f,t=f['t']-t0+off))
    # keep only uploads with intra-upload span < 2h (otherwise single-file test NZBs spanning years)
    for T in (30,120):
        print(f' all {len(ups)} uploads squeezed into {W}s window, T={T}s: time-only',prf(G,cluster_time(G,T)),' time+shape',prf(G,cluster_shape(G,T)))
