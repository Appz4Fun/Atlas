# Oracle skew correction: same realistic world with front-end skew removed.
src=open('sim5_sessions_realistic.py').read().split("rng=random.Random(12)")[0]
exec(src.replace("rng.uniform(-4,4)","0.0"))
rng=random.Random(33)
for P in [8,32]:
    b=[]
    for _ in range(6):
        w=world(P,rng); b.append(score(w,stitch(w,False,True,2)))
    print('noskew',P,' '.join(f'{x:.2f}' for x in np.mean(b,0)),flush=True)
