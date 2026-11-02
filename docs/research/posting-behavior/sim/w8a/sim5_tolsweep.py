import sys; sys.argv=['x']
exec(open('sim5_sessions_realistic.py').read().split("rng=random.Random(12)")[0])
rng=random.Random(21)
print('P tol  overlap+artsize+yEnc-total-veto: prec rec whole')
for P in [8,32]:
    W=[world(P,rng) for _ in range(3)]
    for tol in [0,2,5,10]:
        r=np.mean([score(a,stitch(a,False,True,tol)) for a in W],0)
        print(P,tol,' '.join(f'{x:.3f}' for x in r),flush=True)
