# w7b: zero-download segment-size-sequence index: info content + LSH/k-gram sim
import numpy as np, sys, math, collections
P=716800; base_lines=math.ceil(P/128)
crit=np.zeros(256,np.int64)
for b in range(256):
    if ((b+42)&255) in (0,10,13,61): crit[b]=1
def esc_seq(buf):
    a=np.frombuffer(buf,np.uint8); n=len(a)//P
    return np.array([int(crit[a[i*P:(i+1)*P]].sum()) for i in range(n)])
def renyi2_bits(x):
    c=collections.Counter(x.tolist()); n=len(x)
    if n<2: return 0.0
    pc=sum(v*(v-1) for v in c.values())/(n*(n-1))  # unbiased collision prob
    return -math.log2(pc) if pc>0 else math.log2(n*(n-1))  # lower bound if no collisions
def gauss_bits(x):
    sd=np.std(x); return math.log2(max(sd,1e-9)*math.sqrt(2*math.pi*math.e)) if sd>0 else 0
def rng_bits(x):  # collision bits for Gaussian approx of iid draws = log2(2*sqrt(pi)*sd)
    sd=np.std(x); return math.log2(2*math.sqrt(math.pi)*sd) if sd>0 else 0
def lag_corr(x):
    d=x-x.mean(); return float((d[1:]*d[:-1]).mean()/d.var()) if d.var()>0 else 0
print("== 1. information content per middle part (escape-count deviation) ==")
rs=np.random.default_rng(1)
sets={'random':rs.bytes(P*200),'xz(compressed,real)':open('llvm.xz','rb').read(),
      'ELF libLLVM raw':open('/usr/lib/llvm-18/lib/libLLVM.so.1','rb').read()[:P*170],
      'text/html':open('text.bin','rb').read()}
info={}
for k,b in sets.items():
    s=esc_seq(b)[1:-1]; info[k]=s
    print(f"{k:22s} parts={len(s):4d} mean_esc={s.mean():8.1f} sd={s.std():7.1f} gaussH={gauss_bits(s):5.2f}b "
          f"collisionBits(gauss)={rng_bits(s):5.2f}b renyi2(emp)={renyi2_bits(s):5.2f}b lag1corr={lag_corr(s):+.2f} "
          f"diffseq_sd={np.std(np.diff(s)):.1f}")
# within-file pairwise equal-size prob for random (checks 0.2% figure)
s=info['random']; eq=np.mean(s[:,None]==s[None,:]) - 1/len(s)
print(f"random: empirical P(equal size, two parts) ~ {eq:.4f}")
print("\n== 2. k-gram index: false-candidate model ==")
bits=rng_bits(info['random'])
L=68  # middle parts per 50MB volume
for k in (4,6,8,10,12):
    per_key=k*bits
    for N_stored,N_q in ((1e9*0+1e8,1e7),):
        cmp=(N_stored*L)*(N_q*L)  # key-pair comparisons per day
        fp_content=cmp*2**-per_key; fp_hash=cmp*2**-64
        print(f"k={k:2d} bits/key={per_key:6.1f} content-FP pairs/day={fp_content:.2e} 64bit-hash FP/day={fp_hash:.2e} "
              f"m=2 (independent keys) FP/day~{fp_content**2/ (N_stored*N_q) if fp_content>0 else 0:.2e}")
print("\n-- empirical check at small k with synthetic population (binomial escapes, 200k volumes) --")
Nf=200000; p_esc=4/256
pop=rs.binomial(P,p_esc,size=(Nf,L)).astype(np.int64)
for k in (2,3):
    keys=collections.Counter()
    w=np.zeros((Nf,L-k+1),np.int64)
    for j in range(k): w=w*100003+pop[:,j:j+L-k+1]
    u,c=np.unique(w.ravel(),return_counts=True)
    pairs=int((c*(c-1)//2).sum()); tot=w.size
    pred=tot*(tot-1)/2*2**-(k*bits)
    print(f"k={k}: keys={tot} colliding key-pairs={pairs} predicted={pred:.0f}")
print("\n== 3. recall simulation (k=8, sampled keys, m>=2) ==")
def keys_of(seq,present,k=8,mod=1):
    out=set()
    for i in range(len(seq)-k+1):
        if present[i:i+k].all():
            h=hash(tuple(seq[i:i+k]))&((1<<64)-1)
            if h%mod==0: out.add(h)
    return out
def trial(kind,k=8,m=2,mod=1,T=400,Lm=68):
    hit=0
    for t in range(T):
        full=rs.binomial(P,p_esc,size=Lm+2)
        A=full[1:-1]; pa=np.ones(Lm,bool)
        if kind=='repost': B=A.copy(); pb=pa.copy()
        elif kind=='rerar_mtime': f2=full.copy(); f2[0]+=rs.integers(-50,50); f2[-1]+=rs.integers(-50,50); B=f2[1:-1]; pb=pa.copy()
        elif kind.startswith('missing'):
            q=float(kind.split('_')[1]); B=A.copy(); pb=rs.random(Lm)>q
        elif kind=='unrelated': B=rs.binomial(P,p_esc,size=Lm); pb=pa.copy()
        hit+= len(keys_of(A,pa,k,mod)&keys_of(B,pb,k,mod))>=m
    return hit/T
for mod in (1,4):
    print(f"sampling 1/{mod} of k-grams (content-defined, hash%{mod}==0):")
    for kind in ('repost','rerar_mtime','missing_0.02','missing_0.05','missing_0.10','missing_0.20','unrelated'):
        print(f"   {kind:14s} recall={trial(kind,mod=mod):.3f}")
print("\n== 4. low-info / ngPost gating ==")
for k,s in info.items():
    b=rng_bits(s); kk=math.ceil(72/b) if b>0 else None
    print(f"{k:22s} bits/part={b:5.2f} -> k needed for 72-bit key = {kk}")
ng=np.full(68,P); print("ngPost-style seq: all bytes==716800 -> sd=0, bits=0 -> SKIP")
