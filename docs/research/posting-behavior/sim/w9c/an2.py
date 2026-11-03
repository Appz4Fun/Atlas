import sys,os,re,hashlib,html
d=sys.argv[1];seen=set()
for f in sorted(os.listdir(d),key=lambda x:int(x.split('.')[0]) if x[0].isdigit() else 0):
  if not f.endswith('.nzb'): continue
  b=open(os.path.join(d,f),'rb').read(); h=hashlib.sha1(b).hexdigest()
  if h in seen: continue
  seen.add(h); t=b.decode('latin1')
  files=re.findall(r'<file\b[^>]*subject="([^"]*)"[^>]*>(.*?)</file>',t,re.I|re.S)
  if not files: continue
  segn=len(re.findall(r'<segment\b',t,re.I))
  par2=any('.par2' in html.unescape(s).lower() for s,_ in files)
  if segn>=100 and not par2 or (len(sys.argv)>2 and 'obf' in sys.argv[2]):
    sizes=[sum(int(x) for x in re.findall(r'bytes="(\d+)"',body)) for _,body in files]
    print(f, len(files), segn, html.unescape(files[0][0])[:90], '| sizes', sorted(sizes)[:3], sorted(sizes)[-2:])
