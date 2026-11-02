import os, math, statistics as st
def enc(data, L=128):
    lines=[];cur=bytearray();col=0
    for b in data:
        o=(b+42)&255
        esc = o in (0,10,13,61) or (col==0 and o in (9,32,46)) or (col==L-1 and o in (9,32))
        if esc: cur+=bytes([61,(o+64)&255])
        else: cur.append(o)
        if len(cur)>=L: lines.append(bytes(cur)); cur=bytearray()
        col=len(cur)
    if cur: lines.append(bytes(cur))
    return lines
for L in (128,256):
  for P in (716800,):
    ls=[];bs=[];escs=[]
    for t in range(40):
        d=os.urandom(P); lines=enc(d,L)
        body=sum(len(l)+2 for l in lines); dots=sum(1 for l in lines if l[:1]==b'.')
        ls.append(len(lines)); bs.append(body+dots); escs.append(sum(len(l) for l in lines)-P)
    print(L,P,'lines mean',st.mean(ls),'sd',round(st.pstdev(ls),2),'min',min(ls),'max',max(ls),'bodybytes mean',st.mean(bs),'sd',round(st.pstdev(bs),1),'escfrac',st.mean(escs)/P)
