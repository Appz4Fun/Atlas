# seed stability of pairwise P/R for nyuu K=10 W=0 (gap only and +segment-count)
import sys,statistics
exec(open(sys.argv[1]+'/t2d.py').read().split("print('| K | W s")[0])
for pen in (0,1.0):
    Ps=[];Rs=[]
    for seed in range(5):
        random.seed(seed*31+5); P=[];Rc=[]
        for t in range(100):
            files=[]
            for z in random.sample(zs,10):
                sh=-min(f['start'] for f in bz[z])
                for f in bz[z]: files.append(dict(z=z,start=f['start']+sh,end=f['end']+sh,n=f['n']))
            p,r=pr(files,link(files,2.0,pen)); P.append(p); Rc.append(r)
        Ps.append(sum(P)/len(P)); Rs.append(sum(Rc)/len(Rc))
    print('penalty',pen,'P mean %.3f sd %.3f | R mean %.3f sd %.3f'%(statistics.mean(Ps),statistics.stdev(Ps),statistics.mean(Rs),statistics.stdev(Rs)))
