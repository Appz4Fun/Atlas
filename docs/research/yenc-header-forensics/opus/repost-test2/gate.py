# Entropy-gated twin analysis. Usage: python -I gate.py <nzb dirs...>  (imports corpus parsing by exec of first part)
import sys,re,glob,gzip,hashlib,collections,statistics as st,xml.etree.ElementTree as ET
src=open(__file__.replace('gate.py','corpus.py')).read().split("print(len(paths),'nzbs'")[0]
exec(src)
H=lambda v: hashlib.sha1(repr(v).encode()).hexdigest()[:16]
posts={}
for r in rows: posts.setdefault(frozenset(r['ids']),r)
P=[r for r in posts.values() if r['w']!='flat' and len(r['b'])>=2]
# per-file stats on full (non-last) segments
for r in P:
    f=r['b'][:-1]; r['mean']=st.mean(f); r['sd']=st.pstdev(f) if len(f)>1 else None
art=collections.Counter(round(r['mean'],-3) for r in P)
print('full-seg mean buckets (top):',art.most_common(8))
sds=[r['sd'] for r in P if r['sd'] is not None]
print('sd distribution deciles:',[round(x) for x in st.quantiles(sds,n=10)])
# expected high-entropy mean per article size: median mean among sd in 60..200
hi=collections.defaultdict(list)
for r in P:
    if r['sd'] and 60<=r['sd']<=200: hi[round(r['mean'],-4)].append(r['mean'])
ref={k:st.median(v) for k,v in hi.items() if len(v)>=5}
print('high-entropy reference means:',{k:round(v) for k,v in ref.items()})
def gate(r):
    if r['sd'] is None:  # single full segment: compare to reference mean
        m=ref.get(round(r['mean'],-4)); return m is not None and abs(r["mean"]-m)<1500
    m=ref.get(round(r['mean'],-4))
    return 40<=r["sd"]<=600 and m is not None and abs(r["mean"]-m)<1500
G=[r for r in P if gate(r)]
print(f'eligible {len(P)}  pass entropy gate {len(G)}')
def report(S,label):
    g=collections.defaultdict(list)
    for r in S: g[H(r['b'])].append(r)
    tp=fp=0
    print(f'\n== {label}: full-vector groups with >1 distinct post ==')
    for v in g.values():
        if len(v)<2: continue
        fns={x['fn'] for x in v}; sizes={x['size'] for x in v if x['size']}
        shared=len(set(v[0]['ids'])&set(v[1]['ids']))
        ok=len(fns)==1
        tp+=ok; fp+=not ok
        print(('SAME-NAME' if ok else 'DIFF-NAME'),len(v[0]['b']),'segs sd',round(v[0]['sd'] or -1),'sharedIDs',shared,[(x['nzb'][15:55],x['fn'][:28],x['date']) for x in v][:4])
    print(f'{label}: same-name groups {tp}, diff-name groups {fp}')
    U=list(g.values()); U=[v[0] for v in U]
    for k in (1,2,3,5):
        c=collections.defaultdict(list)
        for r in U:
            if len(r['b'])<=k: continue
            c[(len(r['b']),tuple(r['b'][:k]))].append(r)
        n=sum(len(x) for x in c.values()); coll=sum(len(x) for x in c.values() if len(x)>1)
        c2=collections.defaultdict(list)
        for r in U:
            if len(r['b'])<=k: continue
            c2[tuple(r['b'][:k])].append(r)
        coll2=sum(len(x) for x in c2.values() if len(x)>1)
        print(f'  prefix k={k}: distinct vectors {n}; colliding with segcount {coll} ({coll/max(n,1):.2%}); without segcount {coll2} ({coll2/max(n,1):.2%})')
report(P,'UNGATED')
report(G,'ENTROPY-GATED')
# show gated prefix-k collisions (k=2,5) whose full vectors differ
g={}
for r in G: g.setdefault(H(r['b']),r)
for k in (2,5):
    c=collections.defaultdict(list)
    for r in g.values():
        if len(r['b'])>k: c[(len(r['b']),tuple(r['b'][:k]))].append(r)
    for v in c.values():
        if len(v)>1:
            a,b=v[0],v[1]; eq=sum(x==y for x,y in zip(a['b'],b['b']))
            print('PREFIX-COLL k',k,len(a['b']),'segs eq',eq,a['fn'][:25],a['nzb'][15:50],a['date'],'|',b['fn'][:25],b['nzb'][15:50],b['date'],'sd',round(a['sd'] or -1),round(b['sd'] or -1))
