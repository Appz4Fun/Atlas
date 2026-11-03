# Minimal Nyuu-style yEnc size model (own code): exact encoded article size of a raw segment.
import numpy as np
CRIT=np.zeros(256,bool); CRIT[[0,10,13,61]]=True
EDGE_S=np.zeros(256,bool); EDGE_S[[9,32,46]]=True   # escaped at line start
EDGE_E=np.zeros(256,bool); EDGE_E[[9,32]]=True      # escaped at line end
def enc_chars(raw, ll=128):
    """returns (data_chars, lines, escapes) for raw uint8 array"""
    e=((raw.astype(np.int16)+42)&255).astype(np.uint8)
    w=1+CRIT[e].astype(np.int64)
    C=np.concatenate(([0],np.cumsum(w)))
    n=len(raw); s=0; lines=0; chars=0
    es=EDGE_S[e]&~CRIT[e]; ee=EDGE_E[e]&~CRIT[e]
    while s<n:
        extra=1 if es[s] else 0
        k=int(np.searchsorted(C, C[s]+128-extra, 'left'))  # C[k]-C[s]>=128-extra -> line is bytes s..k-1
        k=min(max(k,s+1),n)
        lc=C[k]-C[s]+extra
        if k-1>s and ee[k-1]: lc+=1
        elif k-1==s and ee[s] and not extra: lc+=1
        chars+=lc; lines+=1; s=k
    return chars,lines,chars-n
def hdr_len(part,total,size,name,begin,end):
    h=f"=ybegin part={part} total={total} line=128 size={size} name={name}\r\n=ypart begin={begin} end={end}\r\n"
    t=f"=yend size={end-begin+1} part={part} pcrc32=00000000\r\n"
    return len(h)+len(t)
def size_vector(data, art, name='x'*32):
    n=len(data); total=-(-n//art); out=[]; esc=[]
    for i in range(total):
        a=i*art; b=min(n,a+art); c,l,e=enc_chars(data[a:b])
        out.append(c+2*l+hdr_len(i+1,total,n,name,a+1,b)); esc.append(e)
    return out,esc
