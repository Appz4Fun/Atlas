import sys, zlib
raw = open('/tmp/claude-0/w6a/selectric.txt','rb').read().decode()
body = raw.split("\n\n",1)[1]
print(repr(body))
for name, b in [("crlf-dot", "Just get another pen\r\n.\r\n"), ("lf", "Just get another pen\n"), ("crlf", "Just get another pen\r\n"), ("crlf-nodot","Just get another pen")]:
    c = zlib.crc32(b.encode()) & 0xffffffff
    print(name, c, "MATCH" if c==3595217085 else "")
