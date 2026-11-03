exec(open('sim5_sessions_realistic.py').read().split("rng=random.Random(12)")[0])
rng=random.Random(33)
print('realistic, 6 seeds, tol=2: P  overlap+size(p r w)  +veto(p r w)')
for P in [8,32]:
    a=[];b=[]
    for _ in range(6):
        w=world(P,rng); a.append(score(w,stitch(w,False,False,2))); b.append(score(w,stitch(w,False,True,2)))
    print(P,' '.join(f'{x:.2f}' for x in np.mean(a,0)),'|',' '.join(f'{x:.2f}' for x in np.mean(b,0)),'| whole sd',f'{np.std([x[2] for x in b]):.2f}',flush=True)
