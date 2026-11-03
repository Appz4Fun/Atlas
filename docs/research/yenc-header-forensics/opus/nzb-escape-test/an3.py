import pickle,statistics as st,itertools,collections,random
rows=pickle.load(open('rows.pkl','rb'))
# dedupe exact same NZB copies (same first msgid)
P=716800;L=128
def hdr(name,part,total,size,b,e):
    s=f"=ybegin part={part} total={total} line={L} size={size} name={name}\r\n=ypart begin={b} end={e}\r\n=yend size={e-b+1} part={part} pcrc32=xxxxxxxx\r\n"
    return len(s.encode())
for r in rows:
    if r['size'] and r['tool']=='nyuu':
        n=len(r['b'])
        r['esc']=[x-(min(P,r['size']-i*P)+2*(-(-min(P,r['size']-i*P)//L)))-hdr(r['fn'],i+1,n,r['size'],i*P+1,min((i+1)*P,r['size']))-3 for i,x in enumerate(r['b'])]
ny=[r for r in rows if 'esc' in r and len(r['b'])>=3]
rand=[r for r in ny if st.pstdev(r['b'][:-1])>30]
print("nyuu files>=3 segs:",len(ny),"random-content:",len(rand))
allesc=[e for r in rand for e in r['esc'][:-1]]
print("escape count per full 716800 part (random content): mean %.1f sd %.1f  model mean=%.0f (1.58%%)"%(st.mean(allesc),st.pstdev(allesc),P*0.0158))
# also line-start/end escapes push mean; theoretical: 4/256 *P = 11200
print("theory 4/256*P=",P*4/256)
# group same (fn,size)
g=collections.defaultdict(list)
for r in ny: g[(r['fn'],r['size'])].append(r)
print("\n== same filename+size groups across NZBs")
for k,v in g.items():
    if len(v)<2: continue
    ids=set(tuple(r['b']) for r in v); 
    print(k[0][:30],k[1],"copies",len(v),"nzbs",sorted(set(r['nzb'][:22] for r in v))[:4],"distinct byte-seqs",len(ids))
# false match among unrelated random files with same n: per-segment exact equality rate of escape counts
pairs=0;eq=0;full=0
for a,b in itertools.combinations(rand,2):
    if (a['fn'],a['size'])==(b['fn'],b['size']): continue
    m=min(len(a['esc']),len(b['esc']))-1
    if m<2: continue
    pairs+=1
    e=sum(a['esc'][i]==b['esc'][i] for i in range(m)); eq+=e; full+= (e==m)
    tot=locals().setdefault('tot',0)
print("\nunrelated random pairs",pairs,"per-segment escape-count equality rate %.5f"%(eq/ max(1,sum(min(len(a['esc']),len(b['esc']))-1 for a,b in itertools.combinations(rand,2) if (a['fn'],a['size'])!=(b['fn'],b['size']) and min(len(a['esc']),len(b['esc']))>2))),"whole-seq equal",full)
print("theory per-seg match ~1/(2*sqrt(pi)*sd) =", 1/(2*3.1416**0.5*st.pstdev(allesc)))
