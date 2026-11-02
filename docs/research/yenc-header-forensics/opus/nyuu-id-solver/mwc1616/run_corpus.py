# usage: python3 run_corpus.py <nzb-dir> <mwc_link binary>  -> one line per NZB with stock Nyuu IDs
import sys,re,os,subprocess,datetime,glob
d,binp=sys.argv[1],sys.argv[2]
for f in sorted(glob.glob(os.path.join(d,'**','*.nzb'),recursive=True)):
    ids=re.findall(r'>([A-Za-z]{24})-(\d{13})@[^<]*</segment>',open(f,encoding='utf-8',errors='replace').read())
    ids=[i for i in ids if all(c.isupper()==(j%2==0) for j,c in enumerate(i[0]))]
    if not ids: continue
    day=datetime.datetime.utcfromtimestamp(min(int(t) for _,t in ids)/1000).strftime('%Y-%m-%d')
    out=subprocess.run([binp,'4'],input=''.join('%s %s\n'%i for i in ids),capture_output=True,text=True,timeout=900).stdout
    g=[l for l in out.splitlines() if l.startswith('group')]
    s=[l for l in out.splitlines() if l.startswith(('SUMMARY','gap'))]
    big=sorted((int(l.split('linked ')[1].split()[0]) for l in g),reverse=True)[:5]
    print(day,'|',os.path.basename(f)[:70],'|',' '.join(s),'| top groups',big,flush=True)
