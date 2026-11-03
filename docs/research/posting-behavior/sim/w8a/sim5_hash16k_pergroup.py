# Round 5 task 1: equal-size RAR volumes, interleaved uploads, PAR2 hash16k tie-break.
import random, numpy as np
def world(K,rng,N=51):
    files=[]
    for u in range(K):
        t=rng.uniform(0,30); sp=rng.uniform(8,40)  # MB/s per upload
        G=rng.randint(2,4); grps=rng.sample(range(8),G)
        for i in range(N):
            sz=100 if i<N-1 else rng.uniform(5,60)
            d=sz/sp*rng.uniform(.9,1.1)
            files.append(dict(u=u,i=i,s=int(t),e=int(t+d),g=rng.choice(grps),sz=round(sz,1)))
            t+=d+rng.uniform(0,.6)
    return files
def chain(files):
    n=len(files); C=np.full((n,n),1e6)
    for a in range(n):
        for b in range(n):
            if a!=b and files[a]['sz']==100:
                dt=files[b]['s']-files[a]['e']
                if 0<=dt<=2: C[a,b]=dt+0.01*(files[a]['g']!=files[b]['g'])
    pairs=sorted((C[a,b],a,b) for a in range(n) for b in range(n) if C[a,b]<1e5)
    nxt={};used=set()
    for _,a,b in pairs:
        if a in nxt or b in used: continue
        nxt[a]=b; used.add(b)
    has_prev=set(nxt.values()); chains=[]
    for a in range(n):
        if a in has_prev: continue
        ch=[a]
        while ch[-1] in nxt: ch.append(nxt[ch[-1]])
        chains.append(ch)
    return chains
def baseline(files,chains,K):
    # sizeChain: whole chains matched to uploads by count+last-file size; equal sizes => ties broken by order
    lab={}
    for ch in chains:
        last=files[ch[-1]]
        cand=[u for u in range(K)]  # without hash, any upload fits size list
        # last-file size is distinctive: pick upload whose final volume size matches, else earliest-start heuristic
        u=last['u'] if last['sz']!=100 else min(cand,key=lambda u:abs(starts[u]-files[ch[0]]['s']))
        for f in ch: lab[f]=u
    return lab
def endpoints(files,chains):
    fetched=0; lab={}
    def rec(seg):
        nonlocal fetched
        a,b=seg[0],seg[-1]; fetched+= 1 if a==b else 2
        if files[a]['u']==files[b]['u'] and files[b]['i']-files[a]['i']==len(seg)-1:
            for f in seg: lab[f]=files[a]['u']
            return
        if len(seg)<=2:
            for f in seg: lab[f]=files[f]['u']
            return
        m=len(seg)//2; fetched-=0; rec(seg[:m]); rec(seg[m:])
    for ch in chains: rec(ch)
    return lab,fetched
rng=random.Random(5); print('K  base  hash_all(b/f)  endpoint+index(acc,b/f)  chainF1')
for K in [1,2,4,8]:
    B=[];E=[];F=[];Q=[]
    for rep in range(6):
        files=world(K,rng); global starts
        starts={u:min(f['s'] for f in files if f['u']==u) for u in range(K)}
        ch=chain(files); lab=baseline(files,ch,K)
        B.append(np.mean([lab[j]==f['u'] for j,f in enumerate(files)]))
        l2,fe=endpoints(files,ch); E.append(np.mean([l2[j]==f['u'] for j,f in enumerate(files)])); F.append(fe/len(files))
        good=sum(1 for c in ch for a,b in zip(c,c[1:]) if files[a]['u']==files[b]['u'] and files[b]['i']==files[a]['i']+1)
        Q.append(good/(len(files)-K))
    print(K,f'{np.mean(B):.3f} 1.000(1.00) {np.mean(E):.3f}({np.mean(F):.3f}) {np.mean(Q):.3f}',flush=True)
# Strategy 'unique-link': keep only links where a is b's sole candidate pred AND b is a's sole candidate succ;
# fetch one hash16k per resulting run (hash gives upload+volume index, run inherits it with offset).
def unique_runs(files):
    n=len(files); S={a:[] for a in range(n)}; P={b:[] for b in range(n)}
    for a in range(n):
        if files[a]['sz']!=100: continue
        for b in range(n):
            if a!=b and files[a]['g']==files[b]['g'] and 0<=files[b]['s']-files[a]['e']<=2: S[a].append(b); P[b].append(a)
    nxt={a:S[a][0] for a in range(n) if len(S[a])==1 and len(P[S[a][0]])==1}
    hp=set(nxt.values()); runs=[]
    for a in range(n):
        if a in hp: continue
        r=[a]
        while r[-1] in nxt: r.append(nxt[r[-1]])
        runs.append(r)
    return runs
print('K  unique-link: acc  bodies/file  (incl 1 PAR2 index body/upload)')
rng=random.Random(78)
for K in [1,2,4,8]:
    A=[];Bd=[]
    for rep in range(8):
        files=world(K,rng); runs=unique_runs(files)
        lab={}
        for r in runs:
            u=files[r[0]]['u']
            for f in r: lab[f]=u
        A.append(np.mean([lab[j]==f['u'] for j,f in enumerate(files)])); Bd.append((len(runs)+K)/len(files))
    print(K,f'{np.mean(A):.3f} {np.mean(Bd):.3f}',flush=True)
