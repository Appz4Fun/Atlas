import sys,re,os,glob,pickle,xml.etree.ElementTree as ET
rows=[]
paths=[p for d in sys.argv[1:] for p in glob.glob(d+'/**/*.nzb',recursive=True)]
for p in paths:
    try: root=ET.parse(p).getroot()
    except Exception: continue
    for f in root.iter():
        if not f.tag.endswith('file'): continue
        subj=f.get('subject','')
        segs=sorted([(int(s.get('number',0) or 0),int(s.get('bytes',0) or 0),(s.text or '').strip()) for s in f.iter() if s.tag.endswith('segment')])
        if not segs: continue
        m=re.search(r'"([^"]+)"',subj); fn=m.group(1) if m else subj[:80]
        m2=re.search(r'yEnc\s*\((\d+)/(\d+)\)\s*(\d+)?',subj)
        size=int(m2.group(3)) if m2 and m2.group(3) else None
        rows.append(dict(nzb=p,fn=fn,size=size,b=[s[1] for s in segs],ids=[s[2] for s in segs],poster=f.get('poster',''),date=f.get('date'),subj=subj))
pickle.dump(rows,open('rows.pkl','wb'))
print(len(paths),'nzbs',len(rows),'files')
