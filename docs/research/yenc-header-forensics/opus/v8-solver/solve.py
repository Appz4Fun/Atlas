import json,sys
M64=(1<<64)-1
d=json.load(open(sys.argv[1])); obs=d['out']; N=int(sys.argv[2]) if len(sys.argv)>2 else 40
# symbolic words: list of 64 masks (bit j of word)
def var(off): return [1<<(off+j) for j in range(64)]
def shl(w,k): return [0]*k+w[:64-k]
def shr(w,k): return w[k:]+[0]*k
def xor(a,b): return [x^y for x,y in zip(a,b)]
def step(s0,s1):
    a=s0; b=s1
    a=xor(a,shl(a,23)); a=xor(a,shr(a,17)); a=xor(a,b); a=xor(a,shr(b,26))
    return b,a   # new state0=old state1, new state1=a
def known_bits(k):
    # x in [k/62,(k+1)/62); mantissa m = state0>>12 (52 bits), x=m/2^52
    lo=-(-k*(1<<52)//62); hi=((k+1)*(1<<52)-1)//62
    bits={}
    for j in range(51,-1,-1):
        if (lo>>j)&1==(hi>>j)&1: bits[j]=(lo>>j)&1
        else: break
    return bits  # bit j of mantissa = bit j+12 of state0
def concrete_step(s0,s1):
    a=s0;b=s1;a^=(a<<23)&M64;a^=a>>17;a^=b;a^=b>>26;return b,a
def solve(obs,N):
    res=[]
    # generation stream symbolic: states g_0..; obs within a block are reversed.
    # unknown: offset o = number of values already consumed from the first block (0..63)
    for o in range(64):
        # observed i -> generation index: block b=(i+o)//64, pos within served p=(i+o)%64 -> gen idx b*64 + 63-p
        gidx=[((i+o)//64)*64+63-((i+o)%64) for i in range(N)]
        need=max(gidx)+1
        s0,s1=var(0),var(64); states=[]
        for g in range(need):
            s0,s1=step(s0,s1); states.append(s0)
        rows=[]
        for i in range(N):
            for j,v in known_bits(obs[i]).items():
                rows.append((states[gidx[i]][j+12],v))
        # gaussian elim
        piv={}
        ok=True
        for m,v in rows:
            for p in sorted(piv,reverse=True):
                pass
            while m:
                h=m.bit_length()-1
                if h in piv: pm,pv=piv[h]; m^=pm; v^=pv
                else: piv[h]=(m,v); break
            else:
                if v: ok=False; break
        if not ok: continue
        if len(piv)<128: continue
        # back substitute
        sol=0
        for h in sorted(piv):
            m,v=piv[h]; rest=m&~(1<<h)
            v^=bin(rest&sol).count('1')&1
            if v: sol|=1<<h
        res.append((o,sol&M64,sol>>64))
    return res
import time;t=time.time()
cands=solve(obs,N); print('consistent full-rank offsets',[c[0] for c in cands],'secs',round(time.time()-t,1))
def predict(o,a,b,n):
    gen=[];s0,s1=a,b
    for g in range(64*5): s0,s1=concrete_step(s0,s1); gen.append(s0)
    return [int(((gen[((i+o)//64)*64+63-((i+o)%64)]>>12)/2**52)*62) for i in range(n)]
good=[c for c in cands if predict(*c,N)==obs[:N]]
print('offsets matching all N training obs',[c[0] for c in good])
for o,a,b in good:
    p=predict(o,a,b,len(obs)); print('offset',o,'predicts all',len(obs),':',p==obs,'mismatch',[i for i in range(len(obs)) if p[i]!=obs[i]])
sys.exit()
# predict all 200 by regenerating
gen=[];s0,s1=a,b
for g in range(64*5): s0,s1=concrete_step(s0,s1); gen.append(s0)
pred=[]
for i in range(len(obs)):
    g=((i+o)//64)*64+63-((i+o)%64); pred.append(int(((gen[g]>>12)/2**52)*62))
print('match all:',pred==obs,'mismatch idx',[i for i,(p,q) in enumerate(zip(pred,obs)) if p!=q])
