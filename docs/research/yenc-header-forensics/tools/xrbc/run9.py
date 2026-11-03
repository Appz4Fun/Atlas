import zlib, hashlib
targets={'7564ea0f':'Highwinds','53ab2750':'gnilink1','8b1f9295':'gnilink2','7c706d3b':'gnilink3','e266a3ae':'gnilink4'}
cands=['iad','newsfe22.iad','newsfe22','POSTED','highwinds-media.com','news.highwinds-media.com','feed-me.highwinds-media.com','not-for-mail','gnilink.net','nwrddc02.gnilink.net','nwrddc02','cyclone1.gnilink.net','nwrdny01.gnilink.net','trnddc02','POSTED!not-for-mail','']
hits=[]
for c in cands:
    for name,v in [('crc32',zlib.crc32(c.encode())&0xffffffff),('adler32',zlib.adler32(c.encode())&0xffffffff)]:
        hits.append((f"{name}:{c}", f"{v:08x}"))
    for h in ['md5','sha1','sha256']:
        d=hashlib.new(h,c.encode()).hexdigest()
        hits.append((f"{h}:{c}", d[:8]))
for label,hx in hits:
    if hx in targets: print("HIT",label,hx,targets[hx])
print("searched",len(hits),"candidate hashes; no hit" if not any(h in targets for _,h in hits) else "")
