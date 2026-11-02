import sys, struct, zlib, hashlib, os
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
SIG5=b'Rar!\x1a\x07\x01\x00'
def vint(b,p):
    r=0;s=0
    while True:
        c=b[p];p+=1;r|=(c&0x7f)<<s;s+=7
        if not c&0x80: return r,p
def aesdec(key,iv,data):
    d=Cipher(algorithms.AES(key),modes.CBC(iv)).decryptor();return d.update(data)+d.finalize()
def parse(path,pw=None,verbose=True):
    b=open(path,'rb').read();out=[]
    if not b.startswith(SIG5): return None
    p=8;key=None;total_data=0
    while p<len(b):
        start=p
        if key:
            iv=b[p:p+16];p+=16
            blk0=aesdec(key,iv,b[p:p+16])
            crc=struct.unpack('<I',blk0[:4])[0]; hs,q=vint(blk0,4); hl=q-4
            full=4+hl+hs; enc_len=(full+15)//16*16
            h=aesdec(key,iv,b[p:p+enc_len]); hdr=h[4:4+hl+hs]
            p+=enc_len; datapos=p
        else:
            crc=struct.unpack('<I',b[p:p+4])[0]; hs,q=vint(b,p+4); hdr=b[p+4:q+hs]; datapos=q+hs; p=datapos
        ok=(zlib.crc32(hdr)&0xffffffff)==crc
        hs2,q=vint(hdr,0); typ,q=vint(hdr,q); fl,q=vint(hdr,q)
        ex=ds=0
        if fl&1: ex,q=vint(hdr,q)
        if fl&2: ds,q=vint(hdr,q)
        info=dict(off=start,type=typ,flags=fl,hdrlen=datapos-start,crcok=ok,datasize=ds)
        if typ==1:
            af,q=vint(hdr,q); info['arcflags']=af
            if af&2: info['volnum'],q=vint(hdr,q)
            # extra
            e=hs2+len(hdr[:0])  # placeholder
            eend=len(hdr); estart=eend-ex if ex else eend
            r=estart
            while r<eend:
                sz,r2=vint(hdr,r); t,r3=vint(hdr,r2)
                if t==1:
                    lf,r4=vint(hdr,r3); info['locflags']=lf
                    if lf&1: info['qo_off'],r4=vint(hdr,r4)
                    if lf&2: info['rr_off'],r4=vint(hdr,r4)
                r=r2+sz
        elif typ in (2,3):
            ff,q=vint(hdr,q); us,q=vint(hdr,q); at,q=vint(hdr,q)
            if ff&2: q+=4
            if ff&4: info['datacrc']=struct.unpack('<I',hdr[q:q+4])[0];q+=4
            ci,q=vint(hdr,q); ho,q=vint(hdr,q); nl,q=vint(hdr,q); info['name']=hdr[q:q+nl].decode('utf8','replace')
            info['unp']=us; info['fileflags']=ff; info['method']=(ci>>7)&7
            if ds: info['actual_data_crc']=zlib.crc32(b[datapos:datapos+ds])&0xffffffff
            if typ==3 and info['name']=='QO':
                info['qo_blob']=b[datapos:datapos+ds]
        elif typ==4:
            v,q=vint(hdr,q); ef,q=vint(hdr,q); kdf=hdr[q];q+=1; salt=hdr[q:q+16];q+=16
            info.update(encver=v,encflags=ef,kdf=kdf)
            if pw:
                key=hashlib.pbkdf2_hmac('sha256',pw.encode(),salt,1<<kdf,32)
        out.append(info)
        p=datapos+ds; total_data+=ds
        if typ==5: info['endflags']=vint(hdr,q)[0]; break
    return b,out,p,total_data
if __name__=='__main__':
    pw=os.environ.get('PW')
    for f in sys.argv[1:]:
        r=parse(f,pw)
        if not r: print(f,'not rar5');continue
        b,hs,end,td=r
        print('==',os.path.basename(f),'size',len(b),'mod16',len(b)%16,'end_at',end,'trailing',len(b)-end,'packed',td,'overhead',len(b)-td)
        for h in hs:
            h=dict(h); qb=h.pop('qo_blob',None)
            print('  ',h, ('QOlen=%d'%len(qb)) if qb is not None else '')
