import re,collections,sys
d=collections.defaultdict(set)
for l in open(sys.argv[1],encoding='utf-8',errors='replace'):
    c=l.split('\t')
    if len(c)<3 or '@fx' not in c[2]: continue
    m=re.search(r'\$([^.@]+)\.[^@]*@(fx\w+)',c[2])
    if m: d[c[0]].add(m.groups())
print(sorted(v for s in d.values() for v in s))
