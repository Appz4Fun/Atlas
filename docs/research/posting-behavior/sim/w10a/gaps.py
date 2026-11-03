import sys,re,collections
f=collections.defaultdict(list)
for line in open(sys.argv[1],encoding='utf-8',errors='replace'):
    c=line.rstrip('\n').split('\t')
    if len(c)<5: continue
    m=re.search(r'-(\d{13})@',c[2])
    if not m or c[3]!='nyuu': continue
    f[(c[0],c[1],c[4])].append(int(m.group(1)))
byn=collections.defaultdict(list)
seen=set()
for (n,d,p),ms in f.items():
    byn[n].append((min(ms),max(ms)))
gaps=[]
for n,fl in byn.items():
    key=tuple(sorted(fl))
    if key in seen: continue
    seen.add(key)
    fl=sorted(fl)
    for a,b in zip(fl,fl[1:]): gaps.append((b[0]-a[1])/1000)
gaps.sort(); N=len(gaps)
print(N, 'median',gaps[N//2],'p90',gaps[int(.9*N)])
for t in (0.5,1,2): print('>',t,sum(g>t for g in gaps)/N)
print('<0', sum(g<0 for g in gaps)/N)
