import sys,os,re,email.utils,base64,struct
rows=[]
for root in sys.argv[1:]:
 for dp,dn,fn in os.walk(root):
  for f in fn:
    p=os.path.join(dp,f)
    data=open(p,'rb').read().decode('latin1')
    for msg in re.split(r'\n(?=From )',data):
      hdr=re.sub(r'\n[ \t]+',' ',msg.split('\n\n',1)[0])
      m=re.search(r'(?im)^Message-ID:\s*<([A-Za-z0-9_-]{32})@([^>]+)>',hdr)
      if not m: continue
      nd=re.search(r'(?im)^NNTP-Posting-Date:\s*(.+)$',hdr) or re.search(r'(?im)^Date:\s*(.+)$',hdr)
      try: t=int(email.utils.parsedate_to_datetime(nd.group(1).strip()).timestamp())
      except: t=0
      h=re.search(r'(?im)^NNTP-Posting-Host:\s*(\S+)',hdr); xt=re.search(r'(?im)^X-Trace:\s*(\S+)',hdr)
      b=bytes(c^0x9d for c in base64.urlsafe_b64decode(m.group(1)))
      w=struct.unpack('<6I',b)
      rows.append((t,m.group(2),h.group(1) if h else '',(xt.group(1)[:6] if xt else ''),w,m.group(1)))
rows=sorted(set(rows))
for t,dom,h,xt,w,i in rows:
  print(t, w[2]-t if t else '', dom[:18].ljust(18), h.ljust(15), xt, ' '.join('%08x'%x for x in w), i)
