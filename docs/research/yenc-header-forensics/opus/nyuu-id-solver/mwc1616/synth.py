# Synthetic stock-Nyuu IDs under V8 4.5 MWC1616 (exact math.js MathRandom). usage: synth.py N [seed]
import sys,random
N=int(sys.argv[1]); random.seed(int(sys.argv[2]) if len(sys.argv)>2 else 1)
r0,r1=random.getrandbits(32),random.getrandbits(32)
def rnd():
    global r0,r1
    r0=(18030*(r0&0xFFFF)+(r0>>16))&0xFFFFFFFF; r1=(36969*(r1&0xFFFF)+(r1>>16))&0xFFFFFFFF
    x=((r0<<16)+(r1&0xFFFF))&0xFFFFFFFF; return x*2.3283064365386962890625e-10
def mid(): return ''.join(chr((65 if j%2==0 else 97)+int(rnd()*26)) for j in range(24))
for _ in range(random.randrange(1000)): rnd()
ts=1500000000000
for i in range(N):
    for _ in range(random.choice([1,1,1,0,2])): mid()   # discarded build-time IDs -> gaps 24/48/72
    print(mid(),ts+i)
