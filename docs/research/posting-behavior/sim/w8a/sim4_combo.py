import sys;sys.path.insert(0,'.')
from sim4_p23 import *
rng=random.Random(2)
W=[build_world(nob,40,50,[4,10,20,50],300+s) for nob in [5,20] for s in range(2)]
for share in [0.1,0.3,0.5]:
    o=[]
    for b,m,t in W: R=counter_split(b,m,share,rng); o.append(score(R,subset_chain(R,b,m),mu_of(m,b)))
    print(f'counterSplit+sizeChain share={share} wholeUp={np.mean(o,0)[2]:.2f}',flush=True)
o=[]
for b,m,t in W: R,_=targeted(b,1); o.append(score(R,subset_chain(R,b,m),mu_of(m,b)))
print(f'targeted pad1+sizeChain wholeUp={np.mean(o,0)[2]:.2f}')
