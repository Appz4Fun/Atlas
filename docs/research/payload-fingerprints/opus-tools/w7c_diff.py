import sys
a=open(sys.argv[1],'rb').read();b=open(sys.argv[2],'rb').read()
n=min(len(a),len(b));d=[i for i in range(n) if a[i]!=b[i]] if n<1e5 else None
import itertools
# fast: compare in chunks
diffs=[];CH=1<<16
for o in range(0,n,CH):
    if a[o:o+CH]!=b[o:o+CH]:
        diffs+= [o+i for i in range(min(CH,n-o)) if a[o+i]!=b[o+i]]
print(sys.argv[1],sys.argv[2],'len',len(a),len(b),'ndiff',len(diffs),'first',diffs[:3],'last',diffs[-3:] if diffs else None,'firstDiff>=32?',(diffs[0]>=32 if diffs else None))
