# Build per-file corpus index from NZBs: writer class, seg count, size-vector hashes; find twins/collisions.
import sys,re,glob,gzip,hashlib,collections,statistics as st,xml.etree.ElementTree as ET
def openz(p): return gzip.open(p) if p.endswith('.gz') else open(p,'rb')
rows=[]
paths=sorted(p for d in sys.argv[1:] for p in glob.glob(d+'/**/*.nzb*',recursive=True) if re.search(r'\.nzb(\.gz)?$',p,re.I))
for p in paths:
    try: root=ET.parse(openz(p)).getroot()
    except Exception as e: print('PARSEFAIL',p,e); continue
    for f in root.iter():
        if not f.tag.endswith('}file') and f.tag!='file': continue
        subj=f.get('subject','')
        segs=sorted((int(s.get('number',0) or 0),int(s.get('bytes',0) or 0),(s.text or '').strip()) for s in f.iter() if s.tag.endswith('segment'))
        if not segs: continue
        m=re.search(r'"([^"]+)"',subj); fn=m.group(1) if m else subj[:80]
        m2=re.search(r'yEnc\s*\((\d+)/(\d+)\)\s*(\d+)?',subj)
        b=[s[1] for s in segs]; ids=[s[2] for s in segs]
        full=b[:-1]
        if len(full)>=2 and len(set(full))==1: w='flat'          # ngPost/raw-size writers: no escape info
        elif any('@nyuu' in i or re.match(r'^[a-z0-9]{12,}\.[0-9]{13}@',i) for i in ids[:1]): w='nyuu'
        elif full and min(full)>780000: w='hdr-incl'
        else: w='other'
        rows.append(dict(nzb=p.split('/dl/')[-1],fn=fn,size=int(m2.group(3)) if m2 and m2.group(3) else None,b=b,ids=ids,w=w,poster=f.get('poster',''),date=f.get('date')))
print(len(paths),'nzbs',len(rows),'file entries')
H=lambda v: hashlib.sha1(repr(v).encode()).hexdigest()[:16]
print('writers',collections.Counter(r['w'] for r in rows))
# dedupe identical posts (same message-id set) -> "distinct posts"
posts={}
for r in rows: posts.setdefault(frozenset(r['ids']),r)
P=list(posts.values()); print('distinct posts (by msgid set):',len(P))
def eligible(r): return r['w']!='flat' and len(r['b'])>=2
E=[r for r in P if eligible(r)]
print('eligible (non-flat, >=2 segs):',len(E))
g=collections.defaultdict(list)
for r in E: g[H(r['b'])].append(r)
print('\n== FULL-VECTOR MATCHES between different posts ==')
for k,v in g.items():
    if len(v)<2: continue
    fns={x['fn'] for x in v}; tag='TRUE?' if len(fns)==1 else 'DIFF-NAME'
    print(tag,len(v[0]['b']),'segs',[ (x['nzb'][:60],x['fn'][:40],x['size'],x['w'],x['date']) for x in v])
print('\n== NEAR MISSES: same filename+seg count (or same name), different vector ==')
byfn=collections.defaultdict(list)
for r in P: byfn[r['fn']].append(r)
nm=0
for fn,v in byfn.items():
    hs={H(x['b']) for x in v}
    if len(v)<2 or len(hs)<2: continue
    nm+=1
    if nm>40: continue
    a=v[0]
    for x in v[1:]:
        if H(x['b'])==H(a['b']): continue
        n=min(len(a['b']),len(x['b'])); eq=sum(a['b'][i]==x['b'][i] for i in range(n))
        d=[x['b'][i]-a['b'][i] for i in range(n)]
        print(f"{fn[:45]:45} segs {len(a['b'])}/{len(x['b'])} w {a['w']}/{x['w']} eq {eq}/{n} dmed {st.median(d) if d else None} size {a['size']}/{x['size']} | {a['nzb'][:40]} vs {x['nzb'][:40]}")
print('near-miss filename groups:',nm)
print('\n== COLLISION RATE of prefix-k key over distinct eligible files ==')
# distinct content proxy: group by full vector; count distinct vectors, then check prefix collisions among them
U=list({H(r['b']):r for r in E}.values())
for k in (1,2,3,5,'full'):
    c=collections.defaultdict(list)
    for r in U:
        if k!='full' and len(r['b'])<k+ (1 if k!='full' else 0): continue   # need k full (non-last) segments
        key=(len(r['b']),tuple(r['b'][:k])) if k!='full' else tuple(r['b'])
        c[key].append(r)
    n=sum(len(v) for v in c.values()); coll=sum(len(v) for v in c.values() if len(v)>1)
    pairs=sum(len(v)*(len(v)-1)//2 for v in c.values())
    ex=[ [x['fn'][:30] for x in v] for v in c.values() if len(v)>1][:4]
    print(f'k={k}: files {n} colliding-files {coll} ({coll/max(n,1):.2%}) colliding pairs {pairs} eg {ex}')
# same without segcount in key
for k in (1,2,3,5):
    c=collections.defaultdict(list)
    for r in U:
        if len(r['b'])<=k: continue
        c[tuple(r['b'][:k])].append(r)
    n=sum(len(v) for v in c.values()); coll=sum(len(v) for v in c.values() if len(v)>1)
    print(f'k={k} (no segcount): files {n} colliding {coll} ({coll/max(n,1):.2%})')
import pickle; pickle.dump(rows,open(sys.argv[0].rsplit('/',1)[0]+'/../repost-test2/rows.pkl','wb')) if False else None
