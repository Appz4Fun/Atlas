import sys;sys.path.insert(0,'.')
from sim4_all import *
for nob in [5,20]:
  for K in ['all',3,1]:
    o=[];o2=[];base=[]
    for s in range(2):
        b,m,t=build_vol(nob,300+s,None if K=='all' else K) if K!='all' else build_world(nob,40,50,[4,10,20,50],300+s)
        R=exact_runs(b); mu=mu_of(m,b)
        o.append(score(R,subset_sum(R,b,m),mu)); o2.append(score(R,subset_chain(R,b,m),mu)); base.append(run_chain(R,m,b))
    o=np.mean(o,0);o2=np.mean(o2,0);bb=np.mean(base,0)
    print(f'obf={nob} uniqueK={K}: subsetsum P={o[0]:.3f} R={o[1]:.3f} wholeUp={o[2]:.2f} | sizeChain wholeUp={o2[2]:.2f} | pairwise-chain wholeUp={bb[2]:.2f}',flush=True)
