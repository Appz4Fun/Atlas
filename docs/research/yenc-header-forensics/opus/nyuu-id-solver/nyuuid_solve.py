# Recover V8 xorshift128+ (state0>>12 double, 64-entry LIFO cache) from stock-Nyuu Message-IDs
# <24 alternating Upper/lower = floor(x*26)>-<13-digit ms>@nyuu ; then regenerate stream and link all IDs.
# usage: python3 nyuuid_solve.py file.nzb   (w9b: includes the w8b 127-shift cache-block alignment fix)
import sys,re,time,itertools
M64=(1<<64)-1
ids=re.findall(r'>([A-Za-z]{24})-(\d{13})@[^<]*</segment>',open(sys.argv[1],encoding='utf-8',errors='replace').read())
order=sorted(range(len(ids)),key=lambda i:(ids[i][1],i))
def draws(s): return [ord(c)-(65 if j%2==0 else 97) for j,c in enumerate(s)]
def var(off): return [1<<(off+j) for j in range(64)]
def shl(w,k): return [0]*k+w[:64-k]
def shr(w,k): return w[k:]+[0]*k
def xor(a,b): return [x^y for x,y in zip(a,b)]
def step(s0,s1):
    a=s0;b=s1;a=xor(a,shl(a,23));a=xor(a,shr(a,17));a=xor(a,b);a=xor(a,shr(b,26));return b,a
def known_bits(k,N=26):
    lo=-(-k*(1<<52)//N); hi=((k+1)*(1<<52)-1)//N; bits={}
    for j in range(51,-1,-1):
        if (lo>>j)&1==(hi>>j)&1: bits[j]=(lo>>j)&1
        else: break
    return bits
def cstep(s0,s1):
    a=s0;b=s1;a^=(a<<23)&M64;a^=a>>17;a^=b;a^=b>>26;return b,a
def unshr(x,k):
    r=x
    for _ in range(64//k+1): r=x^(r>>k)
    return r
def unshl(x,k):
    r=x
    for _ in range(64//k+1): r=x^((r<<k)&M64)
    return r
def cback(n0,n1):
    b=n0; t=n1^b^(b>>26); t=unshr(t,17); a=unshl(t,23); return a,b
s0,s1=var(0),var(64); S=[]
for g in range(192): s0,s1=step(s0,s1); S.append(s0)
def solve(obs):
    n=len(obs); out=[]
    for o in range(64):
        gidx=[((i+o)//64)*64+63-((i+o)%64) for i in range(n)]
        piv={}; ok=True
        for i in range(n):
            for j,v in known_bits(obs[i]).items():
                m=S[gidx[i]][j+12]
                while m:
                    h=m.bit_length()-1
                    if h in piv: pm,pv=piv[h]; m^=pm; v^=pv
                    else: piv[h]=(m,v); break
                else:
                    if v: ok=False
                if not ok: break
            if not ok: break
        if not ok or len(piv)<128: continue
        sol=0
        for h in sorted(piv):
            m,v=piv[h]; v^=bin((m&~(1<<h))&sol).count('1')&1
            if v: sol|=1<<h
        out.append((o,sol&M64,sol>>64))
    return out
def served(a,b,BB,BF):
    st=(a,b)
    for _ in range(64*BB): st=cback(*st)
    out=[]
    for blk in range(BB+BF):
        g=[]
        for _ in range(64): st=cstep(*st); g.append(st[0])
        out.extend(reversed(g))
    return out,BB*64
t=time.time()
print('IDs',len(ids),'unique prefixes',len(set(i[0] for i in ids)))
found=None
for p,q in itertools.permutations(order[:10],2):
    obs=draws(ids[p][0])+draws(ids[q][0])
    C=solve(obs)

    for o,a,b in C:
        ss,base=served(a,b,0,2)
        if [((x>>12)*26)>>52 for x in ss[o:o+48]]==obs: found=(p,q,o,a,b);break
    if found: break
if not found: print('NO SOLUTION (pairs tried) secs',round(time.time()-t,1)); sys.exit()
p,q,o,a,b=found
print('solved from IDs #%d,#%d offset %d state=%016x %016x secs %.1f'%(p,q,o,a,b,time.time()-t))
# Alignment fix (ported from stream_link.refine_alignment, w8b): an anchor whose cache-block boundary is off by a few
# states still reproduces IDs lying inside one block, so linking was partial. Try all 127 boundary shifts and keep the
# one that links the most of the 40 IDs nearest in time to the solved pair.
def count_linked(a,b,idx,nb):
    ss,_=served(a,b,nb,nb); key=bytes(((x>>12)*26)>>52 for x in ss)
    return sum(1 for i in idx if key.find(bytes(draws(ids[i][0])))>=0)
near=sorted(range(len(ids)),key=lambda i:abs(int(ids[i][1])-int(ids[p][1])))[:40]
nb=(len(near)*150)//64+8
st=(a,b)
for _ in range(63): st=cback(*st)
best=(-1,a,b)
for sh in range(-63,64):
    n=count_linked(st[0],st[1],near,nb)
    if n>best[0]: best=(n,st[0],st[1])
    st=cstep(*st)
if (best[1],best[2])!=(a,b): print('alignment refined: anchor now %016x %016x (%d/%d near IDs link)'%(best[1],best[2],best[0],len(near)))
a,b=best[1],best[2]; o=0
blocks=len(ids)*150//64+64   # w9b: ~48 draws/article (+retries), was 24/article which truncated the window
ss,base=served(a,b,blocks,blocks)
st=''.join(chr(65+(((x>>12)*26)>>52)) if True else '' for x in ss)
# render as id strings: alternating case relative to draw start parity unknown -> compare by draw values
vals=[((x>>12)*26)>>52 for x in ss]; key=bytes(vals)
origin=base+o; hits=[]
for i,(r,ts) in enumerate(ids):
    k=key.find(bytes(draws(r)))
    hits.append(None if k<0 else k-origin)
linked=[h for h in hits if h is not None]
print('linked %d/%d IDs into one stream; draw-index range %s..%s'%(len(linked),len(ids),min(linked),max(linked)))
gaps=sorted(set(linked)); d=[y-x for x,y in zip(gaps,gaps[1:])]
from collections import Counter; print('gap histogram (draws between consecutive IDs):',Counter(d).most_common(6))
try: print('predicted next 3 IDs after last:',[''.join(chr((65 if j%2==0 else 97)+vals[origin+gaps[-1]+24*(n+1)+j]) for j in range(24)) for n in range(3)])
except IndexError: pass
