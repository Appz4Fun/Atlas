import re,collections,sys
st=collections.defaultdict(list); farm=[0,0]; ch=collections.Counter()
for l in open(sys.argv[1]):
  f=l.rstrip('\n').split('\t')
  if len(f)<3 or not f[1].isdigit(): continue
  t,m=int(f[1]),f[2]
  r=re.match(r'([0-9a-f]{8})\$(\d+)\$(\d+)\$([0-9a-f]+)(?:\$[0-9a-f]+)?@(.+)',m)
  if r: st[r.group(5)].append((int(r.group(1),16)-t,r.group(2),r.group(3),f[0],r.group(4)))
  if m.endswith('@news.usenet.farm'):
    L=m.split('@')[0]; farm[0]+=1; farm[1]+=bool(re.fullmatch('[A-Za-z]{32}',L)); ch.update(L)
for h,v in st.items():
  d=[x[0] for x in v]; print(h,len(v),'dt min/med/max',min(d),sorted(d)[len(d)//2],max(d),'pids',len(set(x[2] for x in v)),'hh',len(set(x[4] for x in v)),'nzbs',len(set(x[3] for x in v)),'seq',collections.Counter(x[1] for x in v).most_common(2))
print('farm',farm,len(ch),'chars; min/max count',min(ch.values()),max(ch.values()))
