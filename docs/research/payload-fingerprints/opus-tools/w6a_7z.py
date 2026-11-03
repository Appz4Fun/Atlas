import sys,os,glob,shutil,subprocess,zlib
sys.path.insert(0,'/tmp/w6a/bin'); from w6a_exp import parts,P
Z='/tmp/w6a/d/xp/usr/lib/p7zip/7za'
def mk(tag,src,vol='5000k',name='movie.bin',mt=1700000000.0):
    d=f'/tmp/w6a/d/z{tag}';shutil.rmtree(d,ignore_errors=True);os.makedirs(d)
    f=f'{d}/{name}';shutil.copy(src,f);os.utime(f,(mt,mt))
    subprocess.run([Z,'a','-mx0',f'-v{vol}',f'{d}/o.7z',f],check=True,stdout=subprocess.DEVNULL);os.remove(f)
    vs=sorted(glob.glob(f'{d}/o.7z.*')); return [parts(v)[0] for v in vs], b''.join(open(v,'rb').read() for v in vs)
S='/tmp/w6a/d/rnd.bin'
(b,bb)=mk('b',S)
for k,kw in [('mtime',dict(mt=1700000123.0)),('subsec',dict(mt=1700000000.5)),('v4200k',dict(vol='4200k')),('longname',dict(name='A'*66+'.bin'))]:
    x,xb=mk(k,S,**kw); pa=[p for v in b for p in v]; pb=[p for v in x for p in v]; n=min(len(pa),len(pb))
    diff=[i for i in range(len(bb)) if i<64 and bb[i]!=xb[i]]
    print(k,'vols',len(b),len(x),'crc_eq',sum(pa[i][0]==pb[i][0] for i in range(n)),'/',n,'size_eq',sum(pa[i][1]==pb[i][1] for i in range(n)),'concat_len',len(bb),len(xb),'first64 diffbytes',diff,
          'concat prefix equal upto',next((i for i in range(min(len(bb),len(xb))) if bb[i]!=xb[i]),None) if len(diff)==0 else 'sighdr')
    # bytes equal ignoring first 32 bytes up to tail?
    m=min(len(bb),len(xb)); i=32
    j=next((t for t in range(32,m,65536) if bb[t:t+65536]!=xb[t:t+65536]),None); print('   first differing 64k block after sighdr at',j,'of',m)
