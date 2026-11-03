import sys,re,os,glob,gzip,statistics as st,json
R=re.compile(r'<segment[^>]*bytes="(\d+)"[^>]*number="(\d+)"[^>]*>([A-Za-z]{24})-(\d{13})@nyuu</segment>')
F=re.compile(r'<file\b([^>]*)>(.*?)</file>',re.S)
out=[]
for p in sorted(glob.glob(sys.argv[1]+'/**/*.nzb*',recursive=True)):
    try: t=(gzip.open if p.endswith('.gz') else open)(p,'rb').read().decode('utf8','replace')
    except: continue
    if '@nyuu' not in t: continue
    seq=[];fdates=[];poster=None
    for fi,(attr,body) in enumerate(F.findall(t)):
        d=re.search(r'date="(\d+)"',attr); po=re.search(r'poster="([^"]*)"',attr); poster=po and po.group(1)
        segs=[(int(n),int(ms),int(b)) for b,n,_,ms in R.findall(body)]
        segs.sort()
        if d: fdates.append((int(d.group(1)), min(s[1] for s in segs)//1000 if segs else None))
        for n,ms,b in segs: seq.append((fi,n,ms,b))
    if len(seq)<2: out.append((os.path.basename(p)[:40],len(seq),poster));continue
    ms=[s[2] for s in seq]
    inv=sum(1 for a,b in zip(ms,ms[1:]) if b<a)/(len(ms)-1)
    gaps=sorted(b-a for a,b in zip(ms,ms[1:]))
    q=lambda f:gaps[int(f*(len(gaps)-1))]
    dur=(max(ms)-min(ms))/1000; byt=sum(s[3] for s in seq)
    skew=[a-b for a,b in fdates if b is not None]
    # ms-equal duplicates
    dupe=len(ms)-len(set(ms))
    # boundary gaps
    bnd=[seq[i+1][2]-seq[i][2] for i in range(len(seq)-1) if seq[i+1][0]!=seq[i][0]]
    out.append(dict(f=os.path.basename(p)[:38],nseg=len(seq),nfile=len(set(s[0] for s in seq)),inv=round(inv,3),p10=q(.1),med=q(.5),p90=q(.9),dur=dur,MBps=round(byt/1e6/dur,2) if dur>0 else None,skew=sorted(set(skew))[:4],dupe=dupe,bnd_med=sorted(bnd)[len(bnd)//2] if bnd else None,poster=poster,t0=min(ms)//1000,segsz=sorted(set(s[3] for s in seq))[-1]))
for o in out: print(json.dumps(o))
