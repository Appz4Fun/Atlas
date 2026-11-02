import sys,random,time
sys.argv=['x']; sys.path.insert(0,'docs/research/yenc-header-forensics/opus/nyuu-id-solver')
import nyuuid_solve_old2 as M
from pycryptosat import Solver
R=random.Random(11); X,Y=R.getrandbits(64),R.getrandbits(64); o=30
ss=M.gen_stream(X,Y,4)
NIDS=int(sys.argv_[0]) if False else 4
obs=[(n*48+j,ss[o+n*48+j]) for n in range(4) for j in range(24)]
# monkeypatch Solver to add known bits as unit clauses
fix=int(__import__('os').environ.get('FIX','128'))
orig=M.build_and_solve
import pycryptosat
class S2(pycryptosat.Solver):
    pass
def bas(args):
    import pycryptosat as P
    real=P.Solver
    def mk(**kw):
        s=real(**kw); bits=[(X>>i)&1 for i in range(64)]+[(Y>>i)&1 for i in range(64)]
        idx=list(range(128)); random.Random(1).shuffle(idx)
        for i in idx[:fix]: s.add_clause([i+1 if bits[i] else -(i+1)])
        return s
    P.Solver=mk
    try: return orig(args)
    finally: P.Solver=real
t=time.time(); r=bas((obs,o,100)); print('fix',fix,r[1],r[2]==(X,Y) if r[2] else None,'%.1fs'%(time.time()-t))
