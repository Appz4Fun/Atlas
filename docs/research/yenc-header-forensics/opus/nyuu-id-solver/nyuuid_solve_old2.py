# Old-V8 (4.9..7.0, Node <= 11) Nyuu Message-ID state recovery WITHOUT z3: CryptoMiniSat (pycryptosat).
# Model (V8 random-number-generator.h, tags 4.9.385..7.0.276): after each xorshift128+ step,
#   x = ((state0 + state1) & (2^52-1)) / 2^52 ; Nyuu letter = floor(x*26)  (65+.. / 97+.. alternating).
# 64-entry cache filled with generations 0..63, consumed LIFO (draw j of block -> generation 63-j).
# Encoding: the 128 initial state bits are CMS vars; every needed state bit is a native XOR clause over them
# (xorshift is GF(2)-linear -> Gauss-Jordan in CMS); the 64-bit add is bit-blasted only on a window of the
# top bits 51..L with a FREE carry-in at bit L (sound: no approximation); each leak forbids the top-m-bit
# patterns incompatible with floor(x*26)=k (blocking clauses).
# usage: nyuuid_solve_old2.py file.nzb [gaps=48,24,72] [npairs=2] [offsets=all|a-b] [workers=4]
#        nyuuid_solve_old2.py --synth N gap seed   (synthetic old-V8 Nyuu NZB, prints true offset to stderr)
import sys,re,time,random
from multiprocessing import Pool
M64=(1<<64)-1; M52=(1<<52)-1
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
def oldv(st): return (((st[0]+st[1])&M52)*26)>>52
def draws(s): return [ord(c)-(65 if j%2==0 else 97) for j,c in enumerate(s)]
def gidx(i): return (i//64)*64+63-(i%64)       # draw index (from block start) -> generation index

if len(sys.argv)>1 and sys.argv[1]=='--synth':
    N,gap,seed=int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]); R=random.Random(seed)
    st=(R.getrandbits(64),R.getrandbits(64)); cache=[]
    def rnd():
        global st
        if not cache:
            g=[]
            for _ in range(64): st=cstep(*st); g.append(oldv(st))
            cache.extend(g)
        return cache.pop()
    off=R.randrange(64)
    for _ in range(off): rnd()
    sys.stderr.write('true offset %d\n'%((off+gap-24)%64))
    for i in range(N):
        for _ in range(gap-24): rnd()           # discarded build-time ID
        s=''.join(chr((65 if j%2==0 else 97)+rnd()) for j in range(24))
        print('<segment number="%d">%s-%d@nyuu</segment>'%(i+1,s,1500000000000+i))
    sys.exit()

# ---- symbolic xorshift: a word = list of 64 masks (int over 128 vars), bit i of the word
def sym_shl(w,k): return [0]*k+w[:64-k]
def sym_shr(w,k): return w[k:]+[0]*k
def sym_xor(u,v): return [x^y for x,y in zip(u,v)]
def sym_step(s0,s1):
    a=s0;b=s1;a=sym_xor(a,sym_shl(a,23));a=sym_xor(a,sym_shr(a,17));a=sym_xor(a,b);a=sym_xor(a,sym_shr(b,26));return b,a

M_TOP=8; EXTRA=6     # constrain top 8 bits (51..44); adder window starts EXTRA bits lower with free carry-in
def allowed_top(k,m):
    lo=-(-k*(1<<52)//26); hi=((k+1)*(1<<52)-1)//26
    return set(range(lo>>(52-m),(hi>>(52-m))+1))

def build_and_solve(args):
    obs,o,tmo=args       # obs: list of (draw_index_relative_to_offset_origin, letter)
    from pycryptosat import Solver
    S=Solver(threads=1,time_limit=tmo)
    nv=[128]
    def new():
        nv[0]+=1; return nv[0]
    maxg=max(gidx(i+o) for i,_ in obs)
    s0=[1<<i for i in range(64)]; s1=[1<<(64+i) for i in range(64)]
    gens=[]
    for g in range(maxg+1):
        s0,s1=sym_step(s0,s1); gens.append((s0,s1))
    varcache={}
    def bitvar(mask):
        if mask in varcache: return varcache[mask]
        if mask & (mask-1)==0 and mask: v=mask.bit_length()     # single initial var
        else:
            v=new(); S.add_xor_clause([b+1 for b in range(128) if mask>>b&1]+[v],False)
        varcache[mask]=v; return v
    L=52-M_TOP-EXTRA
    for i,k in obs:
        a,b=gens[gidx(i+o)]
        c=new(); sbits={}
        for bit in range(L,52):
            x=bitvar(a[bit]); y=bitvar(b[bit]); sv=new()
            S.add_xor_clause([x,y,c,sv],False)
            if bit<51:
                cn=new()   # cn = maj(x,y,c)
                S.add_clause([-x,-y,cn]);S.add_clause([-x,-c,cn]);S.add_clause([-y,-c,cn])
                S.add_clause([x,y,-cn]);S.add_clause([x,c,-cn]);S.add_clause([y,c,-cn])
                c=cn
            sbits[bit]=sv
        ok=allowed_top(k,M_TOP); top=[sbits[51-j] for j in range(M_TOP)]   # msb first
        for t in range(1<<M_TOP):
            if t in ok: continue
            S.add_clause([(-top[j] if (t>>(M_TOP-1-j))&1 else top[j]) for j in range(M_TOP)])
    t0=time.time(); r,sol=S.solve()
    if r is not True: return (o,str(r),None,time.time()-t0)
    X=sum((1<<i) for i in range(64) if sol[i+1]); Y=sum((1<<i) for i in range(64) if sol[65+i])
    return (o,'sat',(X,Y),time.time()-t0)

def gen_stream(X,Y,nblocks):
    st=(X,Y); out=[]
    for _ in range(nblocks):
        g=[]
        for _ in range(64): st=cstep(*st); g.append(oldv(st))
        out.extend(reversed(g))
    return out

def verify(sol,o,obs):
    X,Y=sol; nb=(max(i for i,_ in obs)+o)//64+2; ss=gen_stream(X,Y,nb)
    return all(ss[i+o]==k for i,k in obs)

import os
NIDS=int(os.environ.get('NIDS','2')); TMO=float(os.environ.get('TMO','120'))
M_TOP=int(os.environ.get('MTOP',M_TOP)); EXTRA=int(os.environ.get('EXTRA',EXTRA))
def solve_pair(dl,G,offsets,workers,tmo=None):
    tmo=tmo or TMO
    obs=[(n*G+j,k) for n,d in enumerate(dl) for j,k in enumerate(d)]
    with Pool(workers) as P:
        for o,r,sol,dt in P.imap_unordered(build_and_solve,[(obs,o,tmo) for o in offsets]):
            print('  offset %2d %s %.1fs'%(o,r,dt),flush=True)
            if sol and verify(sol,o,obs): P.terminate(); return o,sol
    return None

if __name__=='__main__':
    ids=re.findall(r'>([A-Za-z]{24})-(\d{13})@[^<]*</segment>',open(sys.argv[1],encoding='utf-8',errors='replace').read())
    gaps=[int(x) for x in (sys.argv[2] if len(sys.argv)>2 else '48,24,72').split(',')]
    npairs=int(sys.argv[3]) if len(sys.argv)>3 else 2
    offs=sys.argv[4] if len(sys.argv)>4 else 'all'
    offsets=list(range(64)) if offs=='all' else list(range(int(offs.split('-')[0]),int(offs.split('-')[-1])+1))
    workers=int(sys.argv[5]) if len(sys.argv)>5 else 4
    order=sorted(range(len(ids)),key=lambda i:(ids[i][1],i))
    print('IDs',len(ids)); t=time.time(); found=None
    for st_ in range(npairs):
        grp=order[st_:st_+NIDS]; p,q=grp[0],grp[1]
        for G in gaps:
            r=solve_pair([draws(ids[x][0]) for x in grp],G,offsets,workers)
            if r: found=(p,q,G)+r; break
            print(' pair',p,q,'gap',G,'no solution t=%.0fs'%(time.time()-t),flush=True)
        if found: break
    if not found: print('NO SOLUTION secs %.0f'%(time.time()-t)); sys.exit()
    p,q,G,o,(X,Y)=found
    print('SOLVED IDs #%d,#%d gap %d offset %d state=%016x %016x secs %.1f'%(p,q,G,o,X,Y,time.time()-t))
    # link all IDs: go back some blocks and regenerate forward
    nb=len(ids)*96//64+10; st=(X,Y)
    for _ in range(64*nb): st=cback(*st)
    key=bytes(gen_stream(st[0],st[1],2*nb)); origin=nb*64+o; hits=[]
    for r,ts in ids:
        k=key.find(bytes(draws(r))); hits.append(None if k<0 else k-origin)
    linked=sorted(set(h for h in hits if h is not None))
    print('linked %d/%d IDs; draw-index range %s..%s'%(sum(h is not None for h in hits),len(ids),linked[0],linked[-1]))
    from collections import Counter; print('gap histogram:',Counter(y-x for x,y in zip(linked,linked[1:])).most_common(6))
