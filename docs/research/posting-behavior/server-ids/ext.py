import sys,os,re,email.utils
root=sys.argv[1]
out=[]
for dp,dn,fn in os.walk(root):
  for f in fn:
    p=os.path.join(dp,f)
    try: data=open(p,'rb').read().decode('latin1')
    except: continue
    # split into messages crudely on mbox From lines
    for msg in re.split(r'\n(?=From )',data):
      hdr=msg.split('\n\n',1)[0]
      hdr=re.sub(r'\n[ \t]+',' ',hdr)
      mid=re.search(r'(?im)^Message-ID:\s*<([^>]+)>',hdr)
      dt=re.search(r'(?im)^Date:\s*(.+)$',hdr)
      if not mid: continue
      t=''
      if dt:
        try: t=int(email.utils.parsedate_to_datetime(dt.group(1).strip()).timestamp())
        except: pass
      nh=re.search(r'(?im)^NNTP-Posting-Host:\s*(.+)$',hdr); 
      ng=re.search(r'(?im)^(?:X-Complaints-To|Path):\s*(.+)$',hdr)
      fr=re.search(r'(?im)^From:\s*(.+)$',hdr)
      ndate=re.search(r'(?im)^NNTP-Posting-Date:\s*(.+)$',hdr)
      out.append('\t'.join([mid.group(1),str(t),(ndate.group(1).strip() if ndate else ''),(fr.group(1)[:50] if fr else ''),(nh.group(1)[:40] if nh else ''),p[-30:]]))
for o in sorted(set(out)): print(o)
