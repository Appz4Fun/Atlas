import re,glob,gzip,sys,collections
R=re.compile(r'number="(\d+)"[^>]*>[A-Za-z]{24}-(\d{13})@nyuu<')
F=re.compile(r'<file\b([^>]*)>(.*?)</file>',re.S)
seen=set();tot=ok=uniq=0;ovl_pairs=0;pairs=0;within_inv=0;within_n=0;dist=collections.Counter()
allint=[]
for p in sorted(glob.glob(sys.argv[1]+'/**/*.nzb',recursive=True)):
    t=open(p,'rb').read().decode('utf8','replace')
    if '@nyuu' not in t: continue
    files=[]
    for a,b in F.findall(t):
        s=sorted((int(n),int(m)) for n,m in R.findall(b))
        if s: files.append(s)
    if not files: continue
    key=min(x[0][1] for x in files)
    if key in seen: continue
    seen.add(key)
    iv=sorted((min(m for _,m in f),max(m for _,m in f)) for f in files)
    allint+= [(a,b,key) for a,b in iv]
    for i in range(len(iv)-1):
        pairs+=1; ovl_pairs+= iv[i+1][0]<iv[i][1]
    for f in files:
        ms=[m for _,m in f]; within_n+=len(ms)-1; within_inv+=sum(b<a for a,b in zip(ms,ms[1:]))
        # displacement: segment rank by time vs number rank
        order=sorted(range(len(ms)),key=lambda i:ms[i])
        for r,i in enumerate(order): dist[min(abs(r-i),9)]+=1
    if len(iv)>=3:
        for j in range(1,len(iv)-1):
            tot+=1; a,b=iv[j]
            others=iv[:j]+iv[j+1:]
            # predicted hole: between max end of earlier and min start of later
            prev=max(o[1] for o in others if o[0]<=a); nxt=min((o[0] for o in others if o[0]>=a),default=1e20)
            ok+= prev<=a and b<=nxt
print('uploads',len(seen),'adjacent file pairs',pairs,'overlapping(interleaved) pairs',ovl_pairs, round(ovl_pairs/pairs,3))
print('within-file time inversions',within_inv,'/',within_n, round(within_inv/within_n,4))
print('segment displacement (time-rank vs number-rank) hist',sorted(dist.items()))
print('removed-middle-file fits exactly in hole',ok,'/',tot,round(ok/tot,3))
# cross-upload: does any file interval overlap another upload's span?
spans=collections.defaultdict(lambda:[1e20,0])
for a,b,k in allint: s=spans[k]; s[0]=min(s[0],a); s[1]=max(s[1],b)
S=sorted(spans.values()); print('cross-upload span overlaps',sum(S[i+1][0]<S[i][1] for i in range(len(S)-1)),'/',len(S)-1)
