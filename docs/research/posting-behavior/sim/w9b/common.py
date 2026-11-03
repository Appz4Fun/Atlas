import re,collections
A='0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_%'
def hw_t(s):
    v=0
    for i,c in enumerate(s): v+=A.index(c)*64**i
    return v+876645500
HW=re.compile(r'^([0-9a-zA-Z_%]+)\.(\d+)\$([0-9A-Za-z_%]+)\.(\d+)@(fx\d+\.\w+)$')
NY=re.compile(r'^(?:[A-Z][a-z]){12}-(\d{13})@(.+)$')
def load(p):
    rows=[]
    for i,l in enumerate(open(p,encoding='utf-8',errors='replace')):
        f=l.rstrip('\n').split('\t')
        nzb,d,mid,dom,poster=f
        d=int(d) if d.isdigit() else None
        m=NY.match(mid); h=HW.match(mid)
        if m: fam,t='nyuu-style',int(m.group(1))/1000.0; extra=None
        elif h:
            fam='highwinds'; t=float(hw_t(h.group(1))); extra=(h.group(5),h.group(3),int(h.group(2)),int(h.group(4)))
        else: fam,t,extra=('ngPost' if dom=='ngPost' else 'other'),None,None
        rows.append(dict(i=i,nzb=nzb,date=d,mid=mid,dom=dom,poster=poster,fam=fam,t=t,hw=extra))
    return rows
def files(rows):
    """file = maximal run of consecutive rows in same nzb with same NZB date"""
    out=[];cur=None
    for r in rows:
        k=(r['nzb'],r['date'])
        if cur is None or cur['k']!=k:
            cur=dict(k=k,nzb=r['nzb'],date=r['date'],rows=[]); out.append(cur)
        cur['rows'].append(r)
    return out
