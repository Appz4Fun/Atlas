# Old-V8 (<= 7.0, i.e. Node <= 11) variant of nyuuid_solve.py.
# V8 6.x/7.0 ToDouble = (((state0 + state1) & 2^52-1) | 0x3FF0..) - 1  -> x = ((s0+s1) mod 2^52) / 2^52
# (src/base/utils/random-number-generator.h, tags 4.9.385 .. 7.0.276); V8 7.1+ uses state0>>12 (handled by nyuuid_solve.py).
# Same 64-entry LIFO cache. Nonlinear (64-bit add) -> z3 per cache offset.
# usage: nyuuid_solve_old.py file.nzb [gaps=24,48,72] [npairs=4]   |  nyuuid_solve_old.py --synth N gap seed  (writes synthetic nzb to stdout)
import sys,re,time,itertools,random
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

if sys.argv[1]=='--synth':   # own reimplementation of old V8 + Nyuu (build ID + send ID per article)
    N,gap,seed=int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]); R=random.Random(seed)
    st=(R.getrandbits(64),R.getrandbits(64)); cache=[]
    def rnd():
        global st
        if not cache:
            for _ in range(64): st=cstep(*st); cache.append(oldv(st))
        return cache.pop()
    for _ in range(R.randrange(64)): rnd()
    for i in range(N):
        for _ in range(gap-24): rnd()           # discarded build-time ID
        s=''.join(chr((65 if j%2==0 else 97)+rnd()) for j in range(24))
        print('<segment number="%d">%s-%d@nyuu</segment>'%(i+1,s,1500000000000+i))
    sys.exit()

def zsolve(args):
    obs1,obs2,G,o=args
    import z3
    s0,s1=z3.BitVecs('s0 s1',64); a,b=s0,s1; gen=[]
    nb=(o+G+24)//64+1
    for g in range(64*nb):
        x=b; y=a; y=y^(y<<23); y=z3.LShR(y,17)^y; y=y^x^z3.LShR(x,26); a,b=x,y
        gen.append(z3.Extract(51,0,a+b))
    S=z3.Solver(); S.set('timeout',120000)
    pos=list(range(24))+list(range(G,G+24)); obs=obs1+obs2
    for p,k in zip(pos,obs):
        i=p+o; g=(i//64)*64+63-(i%64)
        lo=-(-k*(1<<52)//26); hi=((k+1)*(1<<52)-1)//26
        S.add(z3.UGE(gen[g],lo),z3.ULE(gen[g],hi))
    r=S.check()
    if r!=z3.sat: return (o,G,str(r),None)
    m=S.model(); return (o,G,'sat',(m[s0].as_long(),m[s1].as_long()))

def stream(a,b,BB,BF):
    st=(a,b)
    for _ in range(64*BB): st=cback(*st)
    out=[]
    for blk in range(BB+BF):
        g=[]
        for _ in range(64): st=cstep(*st); g.append(oldv(st))
        out.extend(reversed(g))
    return out,BB*64

if __name__=='__main__':
    ids=re.findall(r'>([A-Za-z]{24})-(\d{13})@[^<]*</segment>',open(sys.argv[1],encoding='utf-8',errors='replace').read())
    gaps=[int(x) for x in (sys.argv[2] if len(sys.argv)>2 else '48,24,72').split(',')]
    npairs=int(sys.argv[3]) if len(sys.argv)>3 else 3
    order=sorted(range(len(ids)),key=lambda i:(ids[i][1],i))
    print('IDs',len(ids)); t=time.time(); found=None
    pairs=list(zip(order,order[1:]))[:npairs]
    with Pool(4) as P:
        for p,q in pairs:
            for G in gaps:
                for o,G_,r,sol in P.imap_unordered(zsolve,[(draws(ids[p][0]),draws(ids[q][0]),G,o) for o in range(64)]):
                    if sol:
                        a,b=sol; ss,_=stream(a,b,0,(o+G+24)//64+2)
                        if ss[o:o+24]==draws(ids[p][0]) and ss[o+G:o+G+24]==draws(ids[q][0]): found=(p,q,G,o,a,b); P.terminate(); break
                if found: break
                print(' pair',p,q,'gap',G,'no solution, t=%.0fs'%(time.time()-t),flush=True)
            if found: break
    if not found: print('NO SOLUTION secs %.0f'%(time.time()-t)); sys.exit()
    p,q,G,o,a,b=found
    print('solved IDs #%d,#%d gap %d offset %d state=%016x %016x secs %.1f'%(p,q,G,o,a,b,time.time()-t))
    blocks=len(ids)*96//64+10
    vals,base=stream(a,b,blocks,blocks); key=bytes(vals); origin=base+o; hits=[]
    for r,ts in ids:
        k=key.find(bytes(draws(r))); hits.append(None if k<0 else k-origin)
    linked=sorted(set(h for h in hits if h is not None))
    print('linked %d/%d IDs; draw-index range %s..%s'%(sum(h is not None for h in hits),len(ids),linked[0],linked[-1]))
    from collections import Counter; print('gap histogram:',Counter(y-x for x,y in zip(linked,linked[1:])).most_common(6))
