import sys,re,glob,json,statistics as st,collections
rows=[json.loads(l) for l in open(sys.argv[1]) if l.startswith('{')]
u={}
for r in rows: u.setdefault(r['t0'],r)
U=list(u.values())
print('nzbs',len(rows),'unique uploads by t0',len(U))
def s(k): v=sorted(x[k] for x in U if x[k] is not None); return k,v[0],v[len(v)//10],v[len(v)//2],v[9*len(v)//10],v[-1]
for k in ['nseg','inv','p10','med','p90','dur','MBps','dupe','bnd_med']: print(*s(k))
print('skew set',collections.Counter(tuple(x['skew']) for x in U))
import datetime
by=collections.defaultdict(list)
for x in U: by[datetime.datetime.utcfromtimestamp(x['t0']).year].append(x)
for y in sorted(by): L=by[y]; print(y,len(L),'medgap',st.median(x['med'] for x in L),'MBps',round(st.median(x['MBps'] for x in L),2),'inv',round(st.mean(x['inv'] for x in L),4),'segsz',collections.Counter(x['segsz']//1000 for x in L).most_common(2))
