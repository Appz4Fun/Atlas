# Adapted from opus/v8-solver/solve.py: recover V8 xorshift128+ state from ONE article's Subject+From (68 chars),
# then regenerate served stream backward+forward (V8 64-entry LIFO cache) and locate every other article's headers.
import json,sys,re,time
M64=(1<<64)-1
CH='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
d=json.load(open(sys.argv[1])); K=int(sys.argv[2]); arts=d['arts']
strip=lambda s: re.sub(r'[^A-Za-z0-9]','',s)
obs=[CH.index(c) for c in arts[K]['subject']+strip(arts[K]['from'])]; assert len(obs)==68
def var(off): return [1<<(off+j) for j in range(64)]
def shl(w,k): return [0]*k+w[:64-k]
def shr(w,k): return w[k:]+[0]*k
def xor(a,b): return [x^y for x,y in zip(a,b)]
def step(s0,s1):
    a=s0;b=s1;a=xor(a,shl(a,23));a=xor(a,shr(a,17));a=xor(a,b);a=xor(a,shr(b,26));return b,a
def known_bits(k):
    lo=-(-k*(1<<52)//62); hi=((k+1)*(1<<52)-1)//62; bits={}
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
def cback(n0,n1):  # inverse of cstep
    b=n0; t=n1^b^(b>>26); t=unshr(t,17); a=unshl(t,23); return a,b
# symbolic states for 2 blocks (68 obs can span at most 2 blocks)
s0,s1=var(0),var(64); S=[]
for g in range(128): s0,s1=step(s0,s1); S.append(s0)
t=time.time(); cands=[]
for o in range(64):
    gidx=[((i+o)//64)*64+63-((i+o)%64) for i in range(68)]
    piv={}; ok=True
    for i in range(68):
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
    cands.append((o,sol&M64,sol>>64))
# verify candidates
def served_stream(a,b,BB=200,BF=200):
    # block 0 starts from state (a,b). go back BB blocks.
    st=(a,b)
    for _ in range(64*BB): st=cback(*st)
    out=[]
    for blk in range(BB+BF):
        g=[]
        for _ in range(64): st=cstep(*st); g.append(st[0])
        out.extend(reversed(g))
    return out, BB*64   # served doubles-as-u64, index of block0 start
good=[]
for o,a,b in cands:
    ss,base=served_stream(a,b,0,2)
    if [((x>>12)*62)>>52 for x in ss[o:o+68]]==obs: good.append((o,a,b))
print('obs article',K,'candidates',[c[0] for c in cands],'verified',[c[0] for c in good],'solve secs',round(time.time()-t,2))
o,a,b=good[0]
ss,base=served_stream(a,b); origin=base+o
chs=''.join(CH[((x>>12)*62)>>52] for x in ss)
def find(s):
    i=chs.find(s)
    if i<0: return None
    assert chs.find(s,i+1)<0
    return i-origin
hits={'subject':0,'from':0,'wire':0,'build':0,'date':0,'first':0}; rows=[]
for n,A in enumerate(arts):
    fs=find(A['subject']); ff=find(strip(A['from'])); fw=find(strip(A['wireMsgId'])); fb=find(strip(A['buildMsgId']))
    # Date: draw directly after From
    dt=None
    if ff is not None:
        x=ss[origin+ff+36]; dt=int(((x>>12)/2**52)*21600000)
    hits['subject']+=fs is not None; hits['from']+=ff is not None; hits['wire']+=fw is not None; hits['build']+=fb is not None
    hits['date']+=dt==A['dateOffMs']
    if 'firstAttemptMsgId' in A: hits['first']+=find(strip(A['firstAttemptMsgId'])) is not None
    rows.append((n,fb,fs,ff,fw))
print('N',len(arts),'Q',d['Q'],'retry',d['RETRY'],'warm',d['WARM'],'hits',hits)
print('per-article draw offsets rel. to observed Subject start (buildMsgId,subject,from,wire):')
print(' '.join(f"{r[0]}:{r[1]},{r[2]},{r[3]},{r[4]}" for r in rows[:12]),'...')
gaps=[rows[i+1][2]-rows[i][2] for i in range(len(rows)-1) if rows[i][2] is not None and rows[i+1][2] is not None]
print('subject-to-next-subject gaps (served draws):',sorted(set(gaps)))
