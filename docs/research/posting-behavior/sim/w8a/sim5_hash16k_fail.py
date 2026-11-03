# Unique-link + hash16k with first-article fetch failures: on failure, fetch the next file in the run
# (its hash gives volume index -> offset back); if whole run fails, fall back to sizeChain guess.
import random, numpy as np
exec(open('sim5_hash16k.py').read().split("rng=random.Random(5)")[0])
exec("def unique_runs"+open('sim5_hash16k.py').read().split("def unique_runs")[1].split("print('K  unique-link")[0])
print('K pfail  acc  bodies/file')
rng=random.Random(9)
for K in [2,8]:
    for pf in [0.02,0.10]:
        A=[];Bd=[]
        for rep in range(6):
            files=world(K,rng); runs=unique_runs(files); lab={}; n=K
            for r in runs:
                u=None
                for f in r:
                    n+=1
                    if rng.random()>pf: u=files[f]['u']; break
                if u is None: u=rng.randrange(K)
                for f in r: lab[f]=u
            A.append(np.mean([lab[j]==f['u'] for j,f in enumerate(files)])); Bd.append(n/len(files))
        print(K,pf,f'{np.mean(A):.3f} {np.mean(Bd):.3f}',flush=True)
