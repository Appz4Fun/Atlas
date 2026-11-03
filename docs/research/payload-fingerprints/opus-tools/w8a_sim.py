# w8a: uniqueness of the NZB-visible RAR5 -hp size tuple (n, S_last) at fixed V; plus PAR2 / 7z extras
import sys, random, math, collections
sys.path.insert(0,'docs/research/payload-fingerprints/opus-tools')
from w7a_hp_model import pred, H
print('check pred(9000123,2000000,33)=',pred(9000123,2000000,33),' L50:',pred(9000123,2000000,50))
V=44040192; N=int(sys.argv[1]) if len(sys.argv)>1 else 1000000
rnd=random.Random(1)
def draw():
    U=int(math.exp(rnd.uniform(math.log(100e6),math.log(20e9)))); L=rnd.randint(20,90); return U,L
pop=[draw() for _ in range(N)]
keys=collections.Counter(pred(U,V,L) for U,L in pop)
uniq=sum(1 for k,c in keys.items() if c==1)/N
pairs=sum(c*(c-1)//2 for c in keys.values())
print(f'N={N} distinct keys={len(keys)} unique frac={uniq:.5f} false pairs={pairs}')
# analytic: S_last spread: count distinct S per n roughly V/1 (S byte-exact? r=pad16(U)-(n-1)D, D not mult of 16 -> S takes ~V/16 values per (n,H) class)
# empirical bits: entropy of key
ent=-sum(c/N*math.log2(c/N) for c in keys.values())
print(f'empirical entropy {ent:.2f} bits (cap log2 N={math.log2(N):.2f})')
# estimate collision prob per random pair: p = sum over n of P(n)^2 * 1/(#S values)
nd=collections.Counter(k[0] for k in keys); 
p=0
for n,c in nd.items():
    pn=sum(v for k,v in keys.items() if k[0]==n)/N
    p+=pn*pn/(V/16)   # S ~uniform over V/16 residue-aligned values
print(f'model per-pair collision p={p:.3e}; expected false pairs @1e7={p*1e14/2:.1f} @1e8={p*1e16/2:.0f}')
# PAR2 add: par2 total = f(slice size, total) ; with fixed slice ~ adds little; extra: 16B-step of S residue already inside
# add name length knowledge? NZB subject shows obfuscated names, so L unknown; with S fully given L is absorbed.
# Mixture: 20% exact-size duplicates of other releases with differing L
dups=0;same=0
for i in range(20000):
    U,L=pop[i]; L2=rnd.randint(20,90)
    if pred(U,V,L)==pred(U,V,L2): same+=1
print(f'same U different L -> same key in {same/20000:.3f} (H changes in 16B steps of L)')
