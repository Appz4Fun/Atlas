import sys,collections;sys.path.insert(0,sys.argv[1])
from common import *
R=load(sys.argv[1]+'/segs.tsv')
G=collections.OrderedDict()
for r in R: G.setdefault((r['nzb'],r['date'],r['poster']),[]).append(r)
c=collections.Counter()
for k,v in G.items():
    t=[r['t'] for r in v if r['fam']=='nyuu-style' and r['date'] and abs(r['t']-r['date'])<86400]
    if not t or len(t)!=len(v): continue
    d=k[1]-int(min(t)); c['floor(first ms)==date' if d==0 else ('date later by %s'%('1' if d==1 else '2-5' if d<=5 else '>5') if d>0 else 'date earlier')]+=1
print(c)
# other family: synthetic-looking dates
o=collections.defaultdict(set)
for r in R:
    if r['fam']=='other': o[r['nzb']].add(r['date'])
rnd=sum(1 for z,ds in o.items() if all(d is None or d%1000==0 for d in ds))
print('other NZBs',len(o),'all dates multiple of 1000',rnd,'single-date NZBs',sum(len(d)==1 for d in o.values()))
print('other domains',collections.Counter(r['dom'] for r in R if r['fam']=='other').most_common(8))
