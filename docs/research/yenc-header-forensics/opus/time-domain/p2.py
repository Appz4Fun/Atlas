import re,sys,glob,gzip,collections,os
def sig(t):
    d={}
    m=re.match(r'\s*(﻿)?<\?xml([^?]*)\?>',t)
    d['decl']=('BOM+' if m and m.group(1) else '')+(re.sub(r'\s+',' ',m.group(2).strip()) if m else 'none')
    dt=re.search(r'<!DOCTYPE[^>]*>',t); d['doctype']=('1.1' if dt and '1.1' in dt.group() else '1.0' if dt and '1.0' in dt.group() else 'other') if dt else 'none'
    x=re.search(r'<nzb\b([^>]*)>',t); d['nzbattr']=re.sub(r'="[^"]*"','',x.group(1)).split() if x else None
    d['meta']=sorted(set(re.findall(r'<meta type="([^"]+)"',t)))
    f=re.search(r'<file\b([^>]*)>',t); d['fileattr']=re.findall(r'(\w+)=',f.group(1)) if f else None
    s=re.search(r'<segment\b([^>]*)>',t); d['segattr']=re.findall(r'(\w+)=',s.group(1)) if s else None
    dv=re.search(r'<file[^>]*date="([^"]*)"',t); d['datefmt']=('unix' if dv and re.fullmatch(r'\d{9,10}',dv.group(1)) else 'other:'+dv.group(1)[:12]) if dv else 'absent'
    ind=re.search(r'\n([ \t]*)<file',t); d['indent']=repr(ind.group(1)) if ind else 'oneline'
    d['crlf']='\r\n' in t
    d['comment']=[c.strip()[:50] for c in re.findall(r'<!--(.*?)-->',t,re.S)][:2]
    mid=re.search(r'<segment[^>]*>([^<]+)</segment>',t)
    if mid:
        v=mid.group(1); dom=v.split('@')[-1] if '@' in v else '(no @)'
        d['msgid']=re.sub(r'[A-Za-z]','a',re.sub(r'\d','9',v.split('@')[0]))[:30]+'@'+dom[:25]
    d['ngroups']=len(re.findall(r'<group>',t[:t.find('</groups>')])) if '</groups>' in t else 0
    d['segclose']='same-line' if re.search(r'<segment[^>]*>[^<]+</segment>',t) else 'split'
    return d
rows=[]
for p in glob.glob(sys.argv[1]+'/**/*',recursive=True):
    if os.path.isdir(p): continue
    try: b=(gzip.open(p).read() if p.endswith('.gz') else open(p,'rb').read())
    except: continue
    t=b.decode('utf-8','replace')
    if '<nzb' not in t or '<segment' not in t: continue
    rows.append((p,sig(t)))
print(len(rows),'nzbs')
cl=collections.defaultdict(list)
for p,d in rows:
    k=(d['decl'],d['doctype'],tuple(d['nzbattr'] or []),tuple(d['fileattr'] or []),tuple(d['segattr'] or []),d['datefmt'][:5],d['indent'],d['crlf'],d['segclose'])
    cl[k].append((p,d))
for k,v in sorted(cl.items(),key=lambda kv:-len(kv[1])):
    doms=collections.Counter(d.get('msgid','').split('@')[-1] for _,d in v).most_common(3)
    metas=collections.Counter(tuple(d['meta']) for _,d in v).most_common(2)
    srcs=collections.Counter(p.split('/')[1] if '/gh/' not in p else p.split('/')[1]+':'+os.path.basename(p).split('_')[0] for p,_ in v).most_common(4)
    com=[d['comment'] for _,d in v if d['comment']][:1]
    print(len(v),'|',' | '.join(map(str,k)),'| dom',doms,'| meta',metas,'| ex',v[0][1].get('msgid'),'| src',srcs,'| cmt',com)
