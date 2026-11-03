import pickle,collections
rows=pickle.load(open('rows.pkl','rb'))
g=collections.defaultdict(list)
for r in rows: g[(r['fn'],len(r['b']))].append(r)
# also key on segment count + total bytes approx
for k,v in g.items():
    if len(v)<2: continue
    idsets=[frozenset(r['ids']) for r in v]
    uniq={}
    for r,s in zip(v,idsets): uniq.setdefault(s,r)
    if len(uniq)<2: continue
    print(k[0][:60],k[1],[ (r['nzb'].split('/')[-1][:30],r['size'],r['ids'][0][-30:]) for r in uniq.values()])
