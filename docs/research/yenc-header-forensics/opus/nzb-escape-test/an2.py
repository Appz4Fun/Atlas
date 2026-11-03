import pickle,statistics as st,itertools,collections
rows=pickle.load(open('rows.pkl','rb'))
P=716800;L=128
def hdr(name,part,total,size,b,e):
    s=f"=ybegin part={part} total={total} line={L} size={size} name={name}\r\n=ypart begin={b} end={e}\r\n"
    s2=f"=yend size={e-b+1} part={part} pcrc32=xxxxxxxx\r\n"
    return len(s.encode())+len(s2.encode())
print("== zero-escape (low-entropy) files: bytes - (P + 2*lines) - modelled yEnc header len")
seen=set()
for r in rows:
    b=r['b']
    if not r['size'] or len(b)<3: continue
    full=b[:-1]
    if st.pstdev(full)>5: continue
    k=(r['fn'],r['size'])
    if k in seen: continue
    seen.add(k)
    n=len(b); res=[]
    for i,x in enumerate(full):
        part=i+1; bb=i*P+1; e=(i+1)*P
        res.append(x-(P+2*(P//L))-hdr(r['fn'],part,n,r['size'],bb,e))
    print(f"{r['nzb'][:28]:28} {r['fn'][:28]:28} namelen={len(r['fn'].encode()):3} residuals uniq={sorted(set(res))}")
