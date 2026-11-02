import math,numpy as np,yenc
def chars_from_size(sz,h):
    x=sz-h; c=x*128/130
    for _ in range(5): c=x-2*math.ceil(c/128.3)
    return c
def predict_vec(vecA,artA,n,artB,name_len=32,offset_known=True):
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
