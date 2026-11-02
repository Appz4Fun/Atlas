# Synthetic tests: (1) per-segment size spread for random data -> n-1 false-hit math; (3) byte-range cross-article-size key.
import sys,math,json,numpy as np; sys.path.insert(0,__file__.rsplit('/',1)[0]); import yenc
R=np.random.default_rng(7); NSEG=int(sys.argv[1]) if len(sys.argv)>1 else 20
A=716800; BS=[768000,750000]
def mk(kind,n):
    d=R.integers(0,256,n,dtype=np.uint8)
    if kind=='reallike':   # compressed-video-like: random with low-escape structured runs (padding, text/index blocks)
        pos=0
        while pos<n:
            pos+=int(R.integers(200_000,2_000_000)); L=int(R.integers(2_000,300_000))
            if pos>=n: break
            if R.random()<0.5: d[pos:pos+L]=0                         # zero padding: (0+42)=42 never escaped
            else: d[pos:pos+L]=R.integers(32,127,min(L,n-pos),dtype=np.uint8)  # ASCII: +42 -> no criticals
            pos+=L
    return d
def chars_from_size(sz,h):
    x=sz-h; c=x*128/130
    for _ in range(5): c=x-2*math.ceil(c/128.3)
    return c
def predict(vecA,artA,n,artB,name_len=32,offset_known=True):
    hA=[yenc.hdr_len(i+1,len(vecA),n,'x'*name_len,i*artA+1,min(n,(i+1)*artA)) for i in range(len(vecA))]
    escA=[chars_from_size(s,h)-(min(n,(i+1)*artA)-i*artA) for i,(s,h) in enumerate(zip(vecA,hA))]
    dens=np.array([e/(min(n,(i+1)*artA)-i*artA) for i,e in enumerate(escA)])
    nb=-(-n//artB); out=[]
    for j in range(nb):
        a,b=j*artB,min(n,(j+1)*artB); E=0.0
        for i in range(a//artA,(b-1)//artA+1):
            lo,hi=max(a,i*artA),min(b,(i+1)*artA); E+=dens[i]*(hi-lo)
        c=(b-a)+E; out.append(c+2*math.ceil(c/128.3)+yenc.hdr_len(j+1,nb,n,'x'*name_len,a+1,b))
    return np.array(out)
def score(pred,actual):
    r=np.array(actual,float)-pred
    return float(np.sqrt(np.mean(r**2))), float(np.sqrt(np.mean((r-np.median(r))**2))), float(r.sum())
res={'seg_sd':[], 'cases':[]}
files={}
for kind in ('random','reallike'):
    for t in range(4):
        n=A*NSEG-int(R.integers(1,A))   # same byte length for all files -> worst case for non-twins
        n=A*NSEG-123457
        d=mk(kind,n); v={A:yenc.size_vector(d,A)[0]}
        for B in BS: v[B]=yenc.size_vector(d,B)[0]
        files[(kind,t)]=(n,v); res['seg_sd'].append((kind,float(np.std(v[A][:-1]))))
        print(kind,t,'A-vector sd',round(np.std(v[A][:-1]),1),flush=True)
for kind in ('random','reallike'):
  for t in range(4):
    n,v=files[(kind,t)]
    for B in BS:
        p=predict(v[A],A,n,B)
        tw=score(p,v[B])
        for t2 in range(4):
            if t2==t: continue
            n2,v2=files[(kind,t2)]
            nt=score(p,v2[B]); res['cases'].append(dict(kind=kind,B=B,twin=False,rms=nt[0],rms_off=nt[1],sum=nt[2]))
        res['cases'].append(dict(kind=kind,B=B,twin=True,rms=tw[0],rms_off=tw[1],sum=tw[2]))
        # reverse direction B->A
        pr=predict(v[B],B,n,A); twr=score(pr,v[A])
        res['cases'].append(dict(kind=kind,B=-B,twin=True,rms=twr[0],rms_off=twr[1],sum=twr[2]))
import collections
agg=collections.defaultdict(list)
for c in res['cases']: agg[(c['kind'],c['B'],c['twin'])].append((round(c['rms'],1),round(c['rms_off'],1),round(c['sum'])))
for k,v in sorted(agg.items(),key=str): print(k, 'rms/rms-offsetfree/sum-resid ->', v[:6])
json.dump(res,open(__file__.rsplit('/',1)[0]+'/synth_out.json','w'),indent=0)
