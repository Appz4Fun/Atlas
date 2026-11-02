import re, collections, statistics as st, sys
sys.path.insert(0, "/tmp/claude-0/-home-claude/a4d53905-78b9-58a1-82a6-938c4e3c790c/scratchpad/w7a/scripts")
from classify import classify, load
ALPHA = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_%"
def t1(prefix):
    v = 0
    for i, ch in enumerate(prefix):
        v += ALPHA.index(ch) * (64 ** i)
    return v + 876645500
RX = re.compile(r"^([0-9A-Za-z_%]{4,6})\.(\d+)\$([0-9A-Za-z_%]+)\.(\d+)@(fx\d+\.[a-z]+\d*)$")
rows = [r for r in load() if classify(r[2]) == "highwinds"]
per = collections.defaultdict(list)
for r in rows:
    m = RX.match(r[2]); assert m
    per[r[0]].append((int(r[1]), m.group(1), int(m.group(2)), m.group(3), int(m.group(4)), m.group(5), r[4]))
print("highwinds segments", len(rows), "NZBs", len(per))
allK = collections.defaultdict(set)
for nzb, L in sorted(per.items(), key=lambda kv: -len(kv[1])):
    L.sort(key=lambda x: x[2])
    date = L[0][0]
    Ns = [x[2] for x in L]
    span = max(Ns) - min(Ns) + 1
    gap = 100.0 * (span - len(set(Ns))) / span
    Bs = collections.Counter(x[3] for x in L)
    dec = [t1(x[1]) - x[0] for x in L]
    Kvals = [x[4] for x in L]
    for k in Kvals: allK[k].add(nzb)
    print(f"{nzb} n={len(L)} date={date} hosts={sorted({x[5] for x in L})} poster={L[0][6][:30]!r} "
          f"N={min(Ns)}..{max(Ns)} gap%={gap:.1f} B={dict(Bs.most_common(3))} "
          f"K[min,med,max]=({min(Kvals)},{int(st.median(Kvals))},{max(Kvals)}) "
          f"T1-date offset(s) min/med/max={min(dec)}/{int(st.median(dec))}/{max(dec)} A_first={L[0][1]} A_last={L[-1][1]}")
rep = {k: v for k, v in allK.items() if len(v) > 1}
print("K values appearing in >1 NZB:", len(rep), "of", len(allK), "distinct K")
print("sample repeats:", list(rep.items())[:5])
