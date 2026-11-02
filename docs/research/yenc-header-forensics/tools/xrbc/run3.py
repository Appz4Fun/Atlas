import sys
for fn, rb, bytesh in [('upd.txt',719,743),('selectric.txt',1367,None)]:
    raw = open('/tmp/claude-0/w6a/'+fn,'rb').read()
    crlf = raw.replace(b"\n", b"\r\n")
    hdr, body = crlf.split(b"\r\n\r\n",1)
    print(fn, "LF len", len(raw), "CRLF len", len(crlf), "CRLF+.CRLF", len(crlf)+3, "body CRLF len", len(body), "X-Received-Bytes", rb, "diff(article-x)", rb-len(crlf))
