import sys,os,zlib,subprocess,glob,shutil,itertools
import numpy as np
sys.path.insert(0,'/tmp/w6a/bin'); import r5
P=716800; LL=128
crit=np.zeros(256,np.int8)
for b in range(256):
    e=(b+42)&255
    if e in (0,10,13,61): crit[b]=1
def enc_size(raw):
    """yEnc encoded body size incl CRLF per line (Nyuu-like edge escaping)."""
    a=np.frombuffer(raw,np.uint8); n=len(a)
    cost=1+crit[a].astype(np.int64); cs=np.concatenate([[0],np.cumsum(cost)])
    e=(a.astype(np.int16)+42)&255
    i=0; out=0; lines=0
    while i<n:
        c0=cost[i]+ (1 if (cost[i]==1 and e[i] in (9,32,46)) else 0)
        col=c0; j=i+1
        if col<LL and j<n:
            # first k>=j with col + cs[k+1]-cs[j] >= LL
            k=int(np.searchsorted(cs, cs[j]+LL-col, 'left'))-1
            if k>=n: k=n-1
            col+=cs[k+1]-cs[j]; j=k+1
        last=j-1
        if last>i and cost[last]==1 and e[last] in (9,32): col+=1
        out+=col+2; lines+=1; i=j
    return out
def parts(path):
    b=open(path,'rb').read()
    return [(zlib.crc32(b[k:k+P])&0xffffffff, enc_size(b[k:k+P])) for k in range(0,len(b),P)], b
def mk(tag,src,rar='rar',vol='5000k',name=None,mtime=1700000000.0):
    d=f'/tmp/w6a/d/{tag}'; shutil.rmtree(d,ignore_errors=True); os.makedirs(d)
    nm=name or 'movie.bin'; f=os.path.join(d,nm); shutil.copy(src,f); os.utime(f,(mtime,mtime))
    subprocess.run([f'/tmp/w5a/bin/{rar}','a','-m0','-ep1','-idq',f'-v{vol}',f'{d}/out.rar',f],check=True,stdout=subprocess.DEVNULL)
    os.remove(f)
    vols=sorted(glob.glob(f'{d}/out*.rar'))
    res=[]
    for v in vols:
        ps,b=parts(v); info=r5.parse(v,verbose=False)[1]
        fh=[x for x in info if x['type']==2][0]
        res.append(dict(parts=ps,raw=b,dstart=fh['off']+fh['hdrlen'],dlen=fh['datasize']))
    return res
def cmp(A,B):
    pa=[p for v in A for p in v['parts']]; pb=[p for v in B for p in v['parts']]
    n=min(len(pa),len(pb)); ec=sum(pa[i][0]==pb[i][0] for i in range(n)); es=sum(pa[i][1]==pb[i][1] for i in range(n))
    # which positions within volume differ
    diffpos=set()
    for va,vb in zip(A,B):
        for k,(x,y) in enumerate(zip(va['parts'],vb['parts'])):
            if x[0]!=y[0]: diffpos.add('first' if k==0 else 'last' if k==len(va['parts'])-1 else f'mid{k}')
    # size-key ambiguity: of matching sizes, how many sizes unique in A
    from collections import Counter
    c=Counter(s for _,s in pa); uniq=sum(1 for _,s in pa if c[s]==1)
    return f"parts {len(pa)}/{len(pb)} crc_eq {ec}/{n} size_eq {es}/{n} diffpos {sorted(diffpos)} uniqueSizesInA {uniq}/{len(pa)}"
def shift(a,n): z=b'\0'*n; return zlib.crc32(z,a)^zlib.crc32(z)
def combine(a,b,n): return shift(a,n)^b
def interval_crc(V,x,y):
    """CRC of inner-file range [x,y) using part CRCs; decode (read raw) only articles that are partial or contain headers. returns crc, ndecoded"""
    crc=0; dec=0; inner=0
    for v in V:
        s=v['dstart']; L=v['dlen']; vlen=len(v['raw'])
        for k,(pc,_) in enumerate(v['parts']):
            ps=k*P; pe=min(ps+P,vlen)
            ds=max(ps,s); de=min(pe,s+L)  # data bytes in this part (vol coords)
            if de<=ds: continue
            i0=inner+ds-s; i1=inner+de-s
            a=max(i0,x); b=min(i1,y)
            if b<=a: continue
            if ds==ps and de==pe and a==i0 and b==i1:
                crc=combine(crc,pc,pe-ps)
            else:
                dec+=1; crc=zlib.crc32(v['raw'][ds+(a-i0):ds+(b-i0)],crc)
        inner+=L
    return crc&0xffffffff,dec
def link(A,B):
    # interval = whole-part span of A's volume 2 (no A decodes)
    v=A[1]; inner0=A[0]['dlen']; s=v['dstart']; L=v['dlen']
    k0=-(-s//P); k1=(s+L)//P
    x=inner0+k0*P-s; y=inner0+k1*P-s
    ca,da=interval_crc(A,x,y); cb,db=interval_crc(B,x,y)
    return f"interval [{x},{y}) len {y-x}: A decodes {da}, B decodes {db}, equal={ca==cb}"
if __name__=='__main__':
    for src in ['rnd','txt']:
        S=f'/tmp/w6a/d/{src}.bin'
        base=mk(src+'base',S)
        V={'mtime':mk(src+'mt',S,mtime=1700000123.0),
           'subsec':mk(src+'ss',S,mtime=1700000000.5),
           'v4200k':mk(src+'v42',S,vol='4200k'),
           'v10000k':mk(src+'v100',S,vol='10000k'),
           'longname':mk(src+'ln',S,name='A'*66+'.bin'),
           'rar7':mk(src+'r7',S,rar='rar7')}
        for k,X in V.items():
            print(src,k,cmp(base,X),'|',link(base,X),flush=True)
