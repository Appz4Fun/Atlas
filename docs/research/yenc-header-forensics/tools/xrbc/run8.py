import zlib,glob,re
for fn in sorted(glob.glob('/tmp/claude-0/w6a/self/*')):
    raw=open(fn,'rb').read()
    m=re.search(rb'X-Received-Body-CRC:\s*(\d+)',raw)
    if not m: print(fn,'no header'); continue
    T=int(m.group(1))
    sep=b'\r\n\r\n' if b'\r\n\r\n' in raw else b'\n\n'
    if sep not in raw: print(fn,'no split'); continue
    body=raw.split(sep,1)[1]
    txt=body.decode('latin1').replace('\r\n','\n')
    lines=txt.split('\n')
    if lines and lines[-1]=='': lines=lines[:-1]
    res=[]
    for lead_name,lead in [('lf-dot',None)]:
        v=('\r\n'.join(lines)+'\r\n.\r\n').encode('latin1')
        c=zlib.crc32(v)&0xffffffff
        res.append(f"crlf_dot={c}{'*MATCH*' if c==T else ''}")
    print(fn.split('/')[-1], 'target',T,'body_lines',len(lines),' '.join(res))
