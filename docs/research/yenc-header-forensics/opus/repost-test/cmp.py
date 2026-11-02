import pickle,collections,statistics as st,itertools
rows=pickle.load(open('rows.pkl','rb'))
g=collections.defaultdict(list)
for r in rows: g[(r['fn'],len(r['b']))].append(r)
out=[]
for k,v in g.items():
    uniq={}
    for r in v:
        if r['size'] is None and all(x==0 for x in r['b']): continue
        uniq.setdefault(frozenset(r['ids']),r)
    u=list(uniq.values())
    for a,b in itertools.combinations(u,2):
        d=[x-y for x,y in zip(a['b'],b['b'])]
        full=d[:-1] if len(d)>1 else d
        eq=sum(1 for x in d if x==0)
        print(f"{k[0][:34]:34} n={k[1]:3} {a['nzb'].split('/')[-1][:22]:22}|{b['nzb'].split('/')[-1][:22]:22} sz={a['size']},{b['size']} eq={eq}/{len(d)} diffs={d[:8]}{'...' if len(d)>8 else ''} sd_full={st.pstdev(full):.1f} meanA={st.mean(a['b']):.0f}")
