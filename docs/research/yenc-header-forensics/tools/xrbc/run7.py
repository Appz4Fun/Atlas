import re
samples={'upd.txt':719,'bookorg.txt':1307,'fo3.txt':1251,'fo3b.txt':1320,'fontorg_tool.txt':1442}
for fn,xrb in samples.items():
    raw=open('/tmp/claude-0/w6a/'+fn,'rb').read()
    lf=raw.replace(b'\r\n',b'\n')
    crlf=lf.replace(b'\n',b'\r\n')
    print(f"{fn:18s} X-Received-Bytes={xrb} crlf_article={len(crlf)} +dot={len(crlf)+3} diff_from_dot={xrb-(len(crlf)+3)} rawLF={len(lf)} bodyCRLFlen={len(crlf.split(b'\r\n\r\n',1)[1]) if b'\r\n\r\n' in crlf else -1}")
