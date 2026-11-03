import sys,re,os,glob,statistics as st,collections,xml.etree.ElementTree as ET
rows=[]
for p in sys.argv[1:]:
    try: root=ET.parse(p).getroot()
    except Exception as e: continue
    for f in root.iter():
        if not f.tag.endswith('file'): continue
        subj=f.get('subject','')
        segs=[(int(s.get('number',0) or 0),int(s.get('bytes',0) or 0),(s.text or '').strip()) for s in f.iter() if s.tag.endswith('segment')]
        if not segs: continue
        segs.sort()
        m=re.search(r'"([^"]+)"',subj); fn=m.group(1) if m else subj[:60]
        m2=re.search(r'yEnc\s*\((\d+)/(\d+)\)\s*(\d+)?',subj)
        size=int(m2.group(3)) if m2 and m2.group(3) else None
        tool=segs[0][2].split('@')[-1][:25]
        rows.append(dict(nzb=os.path.basename(p),fn=fn,size=size,b=[s[1] for s in segs],tool=tool,poster=f.get('poster','')[:30]))
import pickle; pickle.dump(rows,open('rows.pkl','wb'))
print(len(set(r['nzb'] for r in rows)),'nzbs',len(rows),'files',sum(len(r['b']) for r in rows),'segs')
for r in rows:
    b=r['b']; full=b[:-1] if len(b)>1 else []
    if len(full)<3: continue
    print(f"{r['nzb'][:34]:34} {r['fn'][:30]:30} n={len(b):4} size={r['size']} tool={r['tool']:22} mean={st.mean(full):9.1f} sd={st.pstdev(full):7.1f} min={min(full)} max={max(full)}")
