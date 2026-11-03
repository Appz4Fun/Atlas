import sys, re, subprocess, glob, os
W, D, prefix = sys.argv[1], sys.argv[2], (sys.argv[3] if len(sys.argv)>3 else None)
tot=dict(pass_=0, fail=0, missing=0); per=[]
for exp in sorted(glob.glob(os.path.join(D,'*.exp'))):
    arc=exp[:-4]
    m=re.match(r'(.*\.part)(\d+)\.rar$', arc)
    vols=[arc]+(sorted(glob.glob(arc[:-4]+'.r[0-9][0-9]')) if arc.endswith('rar3-old.rar') else [])
    if m:
        n=int(m.group(2))
        while os.path.exists(f'{m.group(1)}{n+1}.rar'): n+=1; vols.append(f'{m.group(1)}{n}.rar')
    args=[W]+([f'--prefix={prefix}'] if prefix else [])+vols
    out=subprocess.run(args,capture_output=True,text=True).stdout
    got={}
    for l in out.splitlines():
        g=re.match(r'\s+ENTRY (?:FILE) name="(.*)" unp=(\d+) pack=\d+ crc=(\S+) os=(\d+).* sa=(\w+)',l)
        if g:
            nm=g.group(1).encode().decode('unicode_escape').encode('latin1').decode('utf8','replace')
            e=got.setdefault(nm,{}); e['dec']=int(g.group(2)); e['os']=int(g.group(4))
            if g.group(5)=='false': e['crc']=g.group(3)
    hdr_enc=' hdr_enc=true' in out
    ents=[]; cur=None
    for l in open(exp,encoding='utf8',errors='replace'):
        if re.match(r'^(R5_)?FILE:',l): cur={}; ents.append(cur)
        if cur is None: continue
        if (g:=re.match(r'\s+name=(.*)$',l)): cur['name']=g.group(1)
        if (g:=re.search(r'os=(\d+):\S+.* dec=(\d+)',l)): cur['os']=int(g.group(1)); cur['dec']=int(g.group(2))
        if (g:=re.search(r'crc=0x([0-9a-f]+)',l)): cur['crc']='%08x'%int(g.group(1),16)
    p=f=mi=0; notes=[]
    for e in ents:
        if 'name' not in e: continue
        gt=got.get(e['name']) or got.get(e['name'].rstrip('/'))
        if gt is None: mi+=1; notes.append('missing:'+e['name']); continue
        for k in ('dec','os','crc'):
            if k in e and k in gt:
                if e[k]==gt[k]: p+=1
                else: f+=1; notes.append(f"{e['name']}.{k} exp={e[k]} got={gt[k]}")
    tot['pass_']+=p; tot['fail']+=f; tot['missing']+=mi
    per.append(f"{os.path.basename(arc):32s} entries={len(ents):3d} pass={p:3d} fail={f:2d} missing={mi:2d}{' HDR_ENCRYPTED' if hdr_enc else ''} {'; '.join(notes[:3])}")
print('\n'.join(per)); print('TOTAL', tot)
