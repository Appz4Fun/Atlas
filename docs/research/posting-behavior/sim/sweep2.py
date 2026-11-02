import sys, random, math, numpy as np
sys.path.insert(0, '.')
from chainsim import *
def link_adapt(files, k, pad, rtol, wtol=0.5):
    by_s = sorted(range(len(files)), key=lambda i: files[i]['s'])
    starts = np.array([files[i]['s'] for i in by_s]); cand=[]
    for i,a in enumerate(files):
        lo = -(k*a['w']/a['rate']) - pad; hi = pad
        for jj in range(np.searchsorted(starts,a['e']+lo), np.searchsorted(starts,a['e']+hi)):
            j=by_s[jj]; b=files[j]
            if j==i or b['s']<=a['s']: continue
            lr=abs(math.log(b['rate']/a['rate']))
            if lr>rtol or abs(b['w']-a['w'])>wtol*max(a['w'],b['w'],4): continue
            exp_gap = -0.5*a['w']/a['rate']
            cand.append((abs(b['s']-a['e']-exp_gap)/(hi-lo)+lr/rtol,i,j))
    cand.sort(); succ={}; pred={}
    for c,i,j in cand:
        if i in succ or j in pred: continue
        succ[i]=j; pred[j]=i
    return succ
rng = random.Random(1)
for nob in [5,20]:
  worlds = [build_world(nob, 40, 50, [4,10,20,50], 300+s) for s in range(3)]
  for ts in ['true','date','rank']:
    F = [[x for x in extract_files(b, m, t, 'all716', ts, 30, rng) if x['n']>1] for b,m,t in worlds]
    for name, fn in [('fixed[-1.5,.5]', lambda f: link(f,-1.5,0.5,0.3,1,0.5)), ('fixed[-3,1]', lambda f: link(f,-3,1,0.3,1,0.5)),
                     ('adapt k=1.2 pad=.5', lambda f: link_adapt(f,1.2,0.5,0.3)), ('adapt k=1.2 pad=1.1', lambda f: link_adapt(f,1.2,1.1,0.3))]:
        r=[score(f, fn(f)) for f in F]
        print(f'obf={nob:2d} {ts:4s} {name:20s} P={np.mean([x[0] for x in r]):.3f} R={np.mean([x[1] for x in r]):.3f} perfect={np.mean([x[2] for x in r]):.2f}')
