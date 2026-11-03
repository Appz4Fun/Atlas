import sys,collections,statistics as st
src=open(__file__.replace('obf.py','corpus.py')).read().split("print(len(paths),'nzbs'")[0]
exec(src)
# all files in obfuscated-rar NZBs: print nzb, fn, size, nsegs, sd, first-id-domain, date, vector head
for r in rows:
    if 'obfuscated-rar-' not in r['nzb'] and 'obfuscated_rar' not in r['nzb'] and 'obfuscated-rar-files' not in r['nzb']: continue
    f=r['b'][:-1]; sd=round(st.pstdev(f)) if len(f)>1 else None
    print(r['nzb'].split('/')[-1][:50],r['fn'][:12],r['size'],len(r['b']),sd,r['date'],r['ids'][0].split('@')[-1][:20],r['b'][:3])
