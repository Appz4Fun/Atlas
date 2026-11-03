import subprocess,sys
V=int(sys.argv[4]) if len(sys.argv)>4 else 100000
def run(U,L,R): return [int(x) for x in subprocess.run(['/tmp/w6b/bin/run.sh',str(U),str(L),str(V),'0',R],capture_output=True,text=True).stdout.split()]
R=sys.argv[1]
for L in range(int(sys.argv[2]),int(sys.argv[3])):
    lo,hi=V-2000,V
    while hi-lo>1:
        m=(lo+hi)//2
        if len(run(m,L,R))==1: lo=m
        else: hi=m
    r=run(hi,L,R); print(L,V-(hi-1),r[-1]-16,flush=True)
