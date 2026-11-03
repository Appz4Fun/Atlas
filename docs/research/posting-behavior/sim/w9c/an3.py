import sys,os,re,hashlib,html
d=sys.argv[1]; L=open(sys.argv[2]).read().split('\n'); seen=set(); st={}
for f in os.listdir(d):
  if not f.endswith('.nzb'): continue
  i=int(f[:-4]); path=L[i-1].lower()
  b=open(os.path.join(d,f),'rb').read(); h=hashlib.sha1(b).hexdigest()
  if h in seen: continue
  seen.add(h); t=b.decode('latin1')
  files=re.findall(r'<file\b[^>]*subject="([^"]*)"[^>]*>(.*?)</file>',t,re.I|re.S)
  if not files: continue
  subs=' '.join(html.unescape(s).lower() for s,_ in files)
  synth=bool(re.search(r'test|fuzz|fixture|seed|sample|example|mock',path)) or 'reftest' in subs
  segn=len(re.findall(r'<segment\b',t,re.I))
  if segn<20: cls='tiny'
  else: cls='synthetic' if synth else 'real'
  par2='.par2' in subs
  names=[re.search(r'"([^"]*)"',html.unescape(s)) for s,_ in files]
  obf= sum(1 for n in names if not n or not re.search(r'\.[a-z0-9]{2,4}$',n.group(1),re.I))>=len(files)/2
  k=(cls,obf); a=st.setdefault(k,[0,0]); a[0]+=1; a[1]+=par2
for k in sorted(st): print(k, 'n=%d par2-visible=%d'%tuple(st[k]))
