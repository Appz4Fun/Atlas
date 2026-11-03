import re,sys,collections
for path in sys.argv[1:]:
    s=open(path,encoding='iso-8859-1').read()
    files=re.findall(r'<file [^>]*date="(\d+)"[^>]*>(.*?)</file>',s,re.S)
    ok=tot=0; doms=collections.Counter(); skew=[]
    for date,body in files:
        for mid in re.findall(r'>([^<\s]+)</segment>',body):
            local,_,dom=mid.partition('@'); doms[dom]+=1
            m=re.search(r'-(\d{13})$',local)
            if m:
                tot+=1; d=int(m.group(1))/1000.0; skew.append(d-int(date))
                if abs(d-int(date))<=1: ok+=1
    skew.sort()
    print(path, "domains",dict(doms.most_common(3)), "ms-stamped",tot, "match_within_1s",ok, "skew_s_min/med/max", (round(skew[0],1),round(skew[len(skew)//2],1),round(skew[-1],1)) if skew else None)
