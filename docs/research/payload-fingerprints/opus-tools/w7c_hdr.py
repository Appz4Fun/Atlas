import sys,struct,lzma
d=b''.join(open(p,'rb').read() for p in sys.argv[1:])
off,size,_=struct.unpack('<QQI',d[12:32]);h=d[32+off:32+off+size]
# parse packPos, packSize and lzma props from 0x17 header (no-AES case)
def num(b,i):
    f=b[i];m=0x80;v=0;i+=1
    for k in range(8):
        if f&m==0: v|=(f&(m-1))<<(8*k);return v,i
        v|=b[i]<<(8*k);i+=1;m>>=1
i=2;pp,i=num(h,i);i+=1;i+=1;ps,i=num(h,i)
j=h.index(bytes.fromhex('5d'));props=h[j:j+5];k=h.index(b'\x0c',j)+1;us,_=num(h,k)
raw=d[32+pp:32+pp+ps]
dec=lzma.LZMADecompressor(lzma.FORMAT_RAW,filters=[{'id':lzma.FILTER_LZMA1,'dict_size':struct.unpack('<I',props[1:])[0],'lc':3,'lp':0,'pb':2}])
out=dec.decompress(raw,us)
print('plain header',len(out),out.hex())
