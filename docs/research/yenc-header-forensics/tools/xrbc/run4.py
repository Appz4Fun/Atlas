import zlib
raw = open('/tmp/claude-0/w6a/h1_master.msg','rb').read()
body = raw.split(b"\n\n",1)[1]
cands = {"body_as_is": body, "body_crlf": body.replace(b"\n", b"\r\n"),
         "body_crlf_dot": body.replace(b"\n", b"\r\n")+b".\r\n",
         "body_lf_dot": body+b".\n", "body_crlf_nodot_strip": body.rstrip(b"\n").replace(b"\n", b"\r\n")}
for k,v in cands.items():
    c = zlib.crc32(v)&0xffffffff
    print(f"{k:24s} len={len(v)} crc={c} {'MATCH' if c==1037710279 else ''}")
print("target X-Received-Bytes 226569, Bytes 226506; actual raw file len", len(raw))
