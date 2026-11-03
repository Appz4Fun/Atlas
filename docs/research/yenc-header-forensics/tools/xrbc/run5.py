import zlib,sys
targets={'bookorg.txt':3344375139,'fo3.txt':3593181288,'fo3b.txt':2521613645,'fontorg_tool.txt':853735325}
for fn,t in targets.items():
    try: raw=open('/tmp/claude-0/w6a/'+fn,'rb').read()
    except Exception as e: print(fn,e); continue
    if b"\n\n" not in raw and b"\r\n\r\n" not in raw: print(fn,"no header/body split", len(raw)); continue
    sep=b"\r\n\r\n" if b"\r\n\r\n" in raw else b"\n\n"
    body=raw.split(sep,1)[1]
    b=body.decode('latin1')
    lines=b.replace('\r\n','\n').split('\n')
    if lines and lines[-1]=='': lines=lines[:-1]
    hits=[]
    for name,v in {'crlf_dot':('\r\n'.join(lines)+'\r\n.\r\n').encode('latin1'),'crlf_nodot':('\r\n'.join(lines)+'\r\n').encode('latin1'),'lf':('\n'.join(lines)+'\n').encode('latin1')}.items():
        c=zlib.crc32(v)&0xffffffff
        hits.append(f"{name}={c}{'*MATCH*' if c==t else ''}")
    print(fn,"target",t,"bodylen",len(body)," ".join(hits))
