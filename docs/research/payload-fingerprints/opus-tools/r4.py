import sys,struct,zlib,os
for f in sys.argv[1:]:
    b=open(f,'rb').read(); p=7; out=[]
    while p+7<=len(b):
        crc,typ,fl,hs=struct.unpack('<HBHH',b[p:p+7])
        add=struct.unpack('<I',b[p+7:p+11])[0] if (fl&0x8000 or typ==0x74) else 0
        if typ==0x74:
            psz,usz,hos,fcrc,ftime,ver,meth,nsz=struct.unpack('<IIBIIBBH',b[p+7:p+7+18+7-3][:25]) if False else (None,)*8
            psz,usz=struct.unpack('<II',b[p+7:p+15]); fcrc=struct.unpack('<I',b[p+16:p+20])[0]; meth=b[p+25]; nsz=struct.unpack('<H',b[p+26:p+28])[0]
            off=32
            if fl&0x100: psz|=struct.unpack('<I',b[p+32:p+36])[0]<<32; off=40
            name=b[p+off:p+off+nsz]
            d=b[p+hs:p+hs+psz]
            out.append((name.decode('latin1')[:30],hex(fl),'meth',meth,'packed',psz,'hdrcrc',fcrc,'chunkcrc',zlib.crc32(d)&0xffffffff,'hs',hs))
        else: out.append((hex(typ),hex(fl),hs,add))
        p+=hs+add
        if typ==0x7b: break
    print('==',os.path.basename(f),len(b),'end',p,'trail',len(b)-p)
    for o in out: print('  ',o)
