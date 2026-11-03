# link-level (next-hop) precision/recall: predicted i->j vs true consecutive files in same NZB
import sys
exec(open(sys.argv[1]+'/t2d.py').read().split("print('| K | W s")[0])
def links(files,maxgap,pen):
    cand=[]
    for i,a in enumerate(files):
        for j,b in enumerate(files):
            g=b['start']-a['end']
            if i!=j and 0<=g<=maxgap: cand.append((g+(pen if a['n']!=b['n'] else 0),i,j))
    cand.sort(); uo=set(); ui=set(); out=set()
    for c,i,j in cand:
        if i in uo or j in ui: continue
        uo.add(i); ui.add(j); out.add((i,j))
    return out
print('| K | W s | penalty | link precision | link recall |')
for K,Wn in ((5,0),(10,0),(20,0),(10,600)):
    for pen in (0,1.0):
        random.seed(7+K+Wn); tp=pp=tt=0
        for t in range(100):
            files=[]
            for z in random.sample(zs,K):
                sh=random.uniform(0,Wn)-min(f['start'] for f in bz[z])
                for f in sorted(bz[z],key=lambda f:f['start']): files.append(dict(z=z,start=f['start']+sh,end=f['end']+sh,n=f['n']))
            true={(i,i+1) for i in range(len(files)-1) if files[i]['z']==files[i+1]['z']}
            pred=links(files,2.0,pen); tp+=len(pred&true); pp+=len(pred); tt+=len(true)
        print('| %d | %d | %.0f | %.3f | %.3f |'%(K,Wn,pen,tp/pp,tp/tt))
