import sys,struct,zlib,os
def rd(path_or_parts):
    parts=path_or_parts
    return b''.join(open(p,'rb').read() for p in parts)
def num(b,i):
    f=b[i];m=0x80;v=0;i+=1
    for k in range(8):
        if f&m==0:
            v|=(f&(m-1))<<(8*k);return v,i
        v|=b[i]<<(8*k);i+=1;m>>=1
    return v,i
def coders(b,i):
    n,i=num(b,i);out=[]
    for _ in range(n):
        fl=b[i];i+=1;idl=fl&0xf;cid=b[i:i+idl].hex();i+=idl
        if fl&0x10: i+=2  # assume simple
        p=b''
        if fl&0x20:
            pl,i=num(b,i);p=b[i:i+pl];i+=pl
        out.append((cid,p))
    return out,i
def main(parts):
    d=rd(parts);tot=len(d)
    sig,ver=d[:6],d[6:8];shcrc,=struct.unpack('<I',d[8:12])
    off,size,crc=struct.unpack('<QQI',d[12:32])
    print(f'total={tot} ver={ver.hex()} NextHeaderOffset={off} NextHeaderSize={size} NHCRC={crc:08x} startCRCok={zlib.crc32(d[12:32])==shcrc}')
    print(f'  check: 32+off+size==total? {32+off+size==tot}; overhead={tot-off}')
    h=d[32+off:32+off+size]
    print('  hdr type',h[0], 'raw hex', h[:80].hex())
    if h[0]==0x17:
        i=1;assert h[i]==6;i+=1
        po,i=num(h,i);nps,i=num(h,i);assert h[i]==9;i+=1;ps,i=num(h,i)
        print(f'  EncodedHeader packPos={po} packSize={ps}')
        while h[i]!=0x0b:i+=1
        i+=1;nf,i=num(h,i);i+=1
        c,i=coders(h,i)
        for cid,p in c:
            if cid=='06f10701':
                fb=p[0];sb=p[1];ncp=fb&0x3f;sl=((fb>>7)&1)+(sb>>4);il=((fb>>6)&1)+(sb&0xf)
                print(f'   AES NumCyclesPower={ncp} salt={p[2:2+sl].hex()} iv={p[2+sl:2+sl+il].hex()}')
            else: print('   coder',cid,p.hex())
        print('  tail remaining', h[i:].hex())
main(sys.argv[1:])
