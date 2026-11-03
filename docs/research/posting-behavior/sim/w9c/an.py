import sys,os,re,hashlib,html,collections
d=sys.argv[1]; segs=set(l.split('\t')[0] for l in open(sys.argv[2]))
rows=[];seen=set()
for f in sorted(os.listdir(d)):
  if not f.endswith('.nzb'): continue
  b=open(os.path.join(d,f),'rb').read()
  h=hashlib.sha1(b).hexdigest(); m5=hashlib.md5(b).hexdigest()
  if h in seen: continue
  seen.add(h)
  t=b.decode('latin1')
  subs=[html.unescape(s) for s in re.findall(r'<file\b[^>]*subject="([^"]*)"',t,re.I)]
  if not subs: continue
  segn=len(re.findall(r'<segment\b',t,re.I))
  insegs = h[:10] in segs or m5[:10] in segs
  low=' '.join(subs).lower()
  par2=any('.par2' in s.lower() for s in subs)
  vol=any(re.search(r'\.vol\d+[+-]\d+\.par2',s,re.I) for s in subs)
  idx=any(re.search(r'\.par2',s,re.I) and not re.search(r'\.vol\d+',s,re.I) for s in subs)
  sfv='.sfv' in low; nfo='.nfo' in low
  rar=bool(re.search(r'\.(rar|r\d\d)\b',low)); z7=bool(re.search(r'\.7z\.\d{3}|\.7z\b',low)); zp='.zip' in low; s001=bool(re.search(r'\.\d{3}"',low)) 
  # obfuscated: subjects without a visible dotted extension
  noext=sum(1 for s in subs if not re.search(r'\.[a-z0-9]{2,4}"?\s*(\(|yEnc|\[|$)',s,re.I) and not re.search(r'"[^"]+\.[A-Za-z0-9]{2,4}"',s))
  rows.append(dict(f=f,n=len(subs),segs=segn,par2=par2,vol=vol,idx=idx,sfv=sfv,nfo=nfo,rar=rar,z7=z7,zp=zp,insegs=insegs,obf=noext==len(subs)))
print('nzbs with files',len(rows),'matched segs hash',sum(r['insegs'] for r in rows))
def rep(sel,name):
  n=len(sel)
  if not n: print(name,0);return
  c=lambda k:sum(r[k] for r in sel)
  print(f"{name}: n={n} par2={c('par2')} ({c('par2')/n:.0%}) idx={c('idx')} vol={c('vol')} sfv={c('sfv')} nfo={c('nfo')} rar={c('rar')} 7z={c('z7')} zip={c('zp')} fullyobf={c('obf')}")
rep(rows,'all')
rep([r for r in rows if r['n']>=3],'>=3 files')
rep([r for r in rows if r['segs']>=100],'>=100 segs')
rep([r for r in rows if r['insegs']],'in segs.tsv')
rep([r for r in rows if r['rar']],'has rar')
rep([r for r in rows if r['par2'] and not r['vol']],'par2 idx only no vol')
