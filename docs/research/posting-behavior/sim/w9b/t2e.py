# penalty sweep for the segment-count proxy, nyuu K=10 W=0
import sys
src=open(sys.argv[1]+'/t2d.py').read().split("print('| K | W s")[0]
exec(src)
print('| penalty s | precision | recall |')
for pen in (0,0.25,0.5,1.0,2.0,1e9):
    random.seed(1000); P=[];Rc=[]
    for t in range(100):
        files=[]
        for z in random.sample(zs,10):
            sh=-min(f['start'] for f in bz[z])
            for f in bz[z]: files.append(dict(z=z,start=f['start']+sh,end=f['end']+sh,n=f['n']))
        p,r=pr(files,link(files,2.0,pen)); P.append(p); Rc.append(r)
    m=lambda a:sum(x for x in a if x==x)/max(1,sum(1 for x in a if x==x))
    print('| %s | %.3f | %.3f |'%('hard (equal only)' if pen>1e8 else pen,m(P),m(Rc)))
