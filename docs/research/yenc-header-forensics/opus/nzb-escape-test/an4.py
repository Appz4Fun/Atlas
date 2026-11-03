import pickle,statistics as st,collections,xml.etree.ElementTree as ET,glob,os
exec(open('scripts/an3.py').read().split('ny=[')[0])
# msgid sets
mid={}
for p in glob.glob('dl/NZBs/*.nzb'):
    try: root=ET.parse(p).getroot()
    except: continue
    for f in root.iter():
        if f.tag.endswith('file'):
            mid[(os.path.basename(p),f.get('subject'))]=1
rand=[r for r in rows if 'esc' in r and len(r['b'])>=3 and st.pstdev(r['b'][:-1])>30 and max(map(abs,r['esc']))<20000]
e=[x for r in rand for x in r['esc'][:-1]]
print("clean random files",len(rand),"escapes/part mean %.1f sd %.1f (model ~11325 / sqrt(P*p*(1-p))=%.0f)"%(st.mean(e),st.pstdev(e),(716800*.0158*.9842)**.5))
g=collections.defaultdict(list)
for r in rows:
    if 'esc' in r and len(r['b'])>=2: g[(r['fn'],r['size'])].append(r)
for k in [('mytestpost.part01.rar',10485760),('postfile.part1.rar',6291456),('somepar2files.vol062+64.par2',3707048),('postfile.part01.rar',10485760),('blabla.bin',104857600),('postfile.vol063+064.par2',3469372)]:
    v=g[k]; print('\n',k)
    for r in v: print('  ',r['nzb'][:30],'esc[:8]=',r['esc'][:8])
