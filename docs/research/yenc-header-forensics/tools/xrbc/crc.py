import sys, zlib, itertools
def variants(body):
    out = {}
    for le_name, le in [("LF","\n"),("CRLF","\r\n"),("CR","\r")]:
        lines = body.split("\n")
        if lines and lines[-1]=="": lines=lines[:-1]
        base = le.join(lines)
        for tn, t in [("noTrail","") ,("trail"+le_name, le)]:
            out[f"{le_name}-{tn}"] = (base + t).encode()
        out[f"{le_name}-trail-dot"] = (base+le+".\r\n").encode()
    return out
def test(label, raw_body, target):
    print(label, "target", target)
    for k,v in variants(raw_body).items():
        c = zlib.crc32(v) & 0xffffffff
        print(f"  {k:22s} len={len(v):5d} crc={c:10d} {'MATCH' if c==target else ''}")
