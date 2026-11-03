# Combined: run "python3 -I w5b_size_uniqueness.py pypi <dir_of_simple_json>" for the PyPI part, else DAT dirs.
import sys
if len(sys.argv) > 2 and sys.argv[1] == "pypi":
    sys.argv = sys.argv[1:]; exec(open(__file__).read().split("#==" + "PYPI==")[-1]); raise SystemExit
"""Empirical uniqueness of exact file size as a linking key.
Input: clrmamepro-style DATs (libretro-database metadat/ + dat/), rows with size/crc/sha1|md5.
Usage: python3 -I w5b_size_uniqueness.py <dat_dir> [...]"""
import os, re, sys, collections, bisect
ROM = re.compile(r'rom \( name "(?P<n>[^"]*)" size (?P<s>\d+)(?: crc (?P<c>[0-9A-Fa-f]{8}))?(?: md5 (?P<m>[0-9A-Fa-f]{32}))?(?: sha1 (?P<h>[0-9A-Fa-f]{40}))?')
files = {}  # identity (sha1|md5|crc+size) -> (size, crc, name, src)
for root in sys.argv[1:]:
    for dp, _, fns in os.walk(root):
        for fn in fns:
            if not fn.endswith('.dat'): continue
            for line in open(os.path.join(dp, fn), encoding='utf-8', errors='replace'):
                m = ROM.search(line)
                if not m: continue
                s = int(m['s']); c = (m['c'] or '').upper()
                ident = (m['h'] or m['m'] or f"{s}:{c}").upper()
                if ident not in files: files[ident] = (s, c, m['n'], fn)
print("distinct files (by sha1/md5):", len(files))
BANDS = [(0, 10e6, '<10MB'), (10e6, 100e6, '10-100MB'), (100e6, 1e9, '100MB-1GB'), (1e9, 5e9, '1-5GB'), (5e9, 1e30, '>5GB')]
by_size = collections.defaultdict(list)
for ident, (s, c, n, src) in files.items(): by_size[s].append((ident, c, n, src))
sizes_sorted = sorted(by_size)
def band(s):
    for lo, hi, nm in BANDS:
        if lo <= s < hi: return nm
print("| band | files | unique-size frac | mean files sharing size | max share | (size,crc) collisions w/ diff hash | unique @16B round | unique @+-300B |")
print("|---|---|---|---|---|---|---|---|")
r16 = collections.Counter(s // 16 for s in by_size for _ in by_size[s])
for lo, hi, nm in BANDS:
    fl = [(s, x) for s in sizes_sorted if lo <= s < hi for x in by_size[s]]
    if not fl: continue
    N = len(fl)
    uniq = sum(1 for s, x in fl if len(by_size[s]) == 1)
    share = sum(len(by_size[s]) for s, x in fl) / N
    mx = max(len(by_size[s]) for s, x in fl)
    sc = collections.Counter((s, x[1]) for s, x in fl if x[1])
    coll = sum(v - 1 for v in sc.values() if v > 1)
    u16 = sum(1 for s, x in fl if r16[s // 16] == 1)
    # +-300B window: count files whose size is within 300B of any other file
    uw = 0
    for s, x in fl:
        if len(by_size[s]) > 1: continue
        i = bisect.bisect_left(sizes_sorted, s)
        nb = (i > 0 and s - sizes_sorted[i-1] <= 300) or (i+1 < len(sizes_sorted) and sizes_sorted[i+1] - s <= 300)
        uw += not nb
    print(f"| {nm} | {N} | {uniq/N:.3f} | {share:.1f} | {mx} | {coll} | {u16/N:.3f} | {uw/N:.3f} |")
print("\nTop shared sizes >=10MB:")
big = sorted(((len(v), s) for s, v in by_size.items() if s >= 10e6), reverse=True)[:12]
for k, s in big:
    srcs = collections.Counter(x[3] for x in by_size[s]).most_common(2)
    print(f"  {s} ({s/2**20:.1f} MiB, s%2048={s%2048}, pow2={s & (s-1) == 0}) x{k} {srcs}")
pw = sum(1 for s in by_size for _ in by_size[s] if s >= 1e6 and s & (s-1) == 0)
print("files >=1MB at exact 2^n:", pw)
s2048 = sum(1 for s in by_size for _ in by_size[s] if s >= 10e6 and s % 2048 == 0)
tot10 = sum(1 for s in by_size for _ in by_size[s] if s >= 10e6)
print(f"files >=10MB that are multiples of 2048 (sector-aligned): {s2048}/{tot10}")
# excluding fixed-size-medium sources: per-source unique fraction for >=100MB
print("\nPer-source unique-size fraction (files >=100MB, sources with >=200 such files):")
srcc = collections.defaultdict(lambda: [0, 0])
for s, v in by_size.items():
    if s < 1e8: continue
    for x in v:
        srcc[x[3]][0] += 1; srcc[x[3]][1] += len(v) == 1
for k, (n, u) in sorted(srcc.items(), key=lambda t: -t[1][0]):
    if n >= 200: print(f"  {k}: n={n} unique={u/n:.3f}")
# (size,crc) collisions detail
print("\n(size,crc) collisions with different sha1:")
sc = collections.defaultdict(list)
for ident, (s, c, n, src) in files.items():
    if c: sc[(s, c)].append((n, src))
for k, v in sc.items():
    if len(v) > 1: print(" ", k, v[:3])
# 'free-form' sizes: drop capacity sizes (shared by >=20 files) and sector-padded multiples of 2048
print("\nFree-form sizes (exclude sizes shared by >=20 files and multiples of 2048):")
for lo, hi, nm in BANDS:
    fl = [s for s in sizes_sorted if lo <= s < hi and len(by_size[s]) < 20 and s % 2048 for _ in by_size[s]]
    if not fl: continue
    u = sum(1 for s in fl if len(by_size[s]) == 1)
    exp = len(set(fl)); span = (min(hi, max(fl)+1) - lo)
    lam = len(fl) / span  # density per byte
    print(f"  {nm}: n={len(fl)} unique={u/len(fl):.3f}  uniform-model P(other within +-300B)={1-pow(2.718281828,-lam*600):.4f}")
# Are free-form >=100MB size collisions the same title (region/revision variants)?
same = diff = 0; ex = []
for s in sizes_sorted:
    v = by_size[s]
    if s < 1e8 or not (1 < len(v) < 20) or s % 2048 == 0: continue
    t = {re.split(r' \(', x[2])[0].lower() for x in v}
    if len(t) == 1: same += 1
    else:
        diff += 1
        if len(ex) < 4: ex.append((s, [x[2] for x in v][:3]))
print(f"\nFree-form >=100MB shared sizes: same title={same}, different titles={diff}")
for e in ex: print("  ", e)
# Encrypted/compressed free-form analog: PSN .pkg / non-sector-aligned files >=10MB from digital-distribution DATs
print("\nDigital-distribution (encrypted pkg/cia/nsp-like) files >=10MB, observed vs uniform model:")
for srcname in sorted({x[3] for v in by_size.values() for x in v}):
    if not any(k in srcname for k in ('PSN', 'Digital', 'eShop', 'Vita')): continue
    fl = sorted(s for s, v in by_size.items() if s >= 10e6 for x in v if x[3] == srcname)
    if len(fl) < 100: continue
    exact = sum(1 for i, s in enumerate(fl) if (i and fl[i-1] == s) or (i+1 < len(fl) and fl[i+1] == s))
    w300 = sum(1 for i, s in enumerate(fl) if (i and s - fl[i-1] <= 300) or (i+1 < len(fl) and fl[i+1] - s <= 300))
    lo, hi = fl[0], fl[-1]
    import math
    # log-uniform density approx: local density = n / (s * ln(hi/lo))
    expw = sum(1 - math.exp(-len(fl) / (s * math.log(hi / lo)) * 600) for s in fl) / len(fl)
    print(f"  {srcname}: n={len(fl)} exact-share={exact/len(fl):.3f} within300B={w300/len(fl):.3f} model300B={expw:.4f} mult2048={sum(s%2048==0 for s in fl)/len(fl):.2f}")
raise SystemExit
#==PYPI==
# PyPI sample part: free-form compressed archive sizes (wheel/sdist), identity=sha256
import json, os, sys, collections, bisect
d = sys.argv[1]; files = {}
for fn in os.listdir(d):
    try: j = json.load(open(os.path.join(d, fn)))
    except Exception: continue
    for f in j.get('files', []):
        s = f.get('size'); h = f.get('hashes', {}).get('sha256')
        if s and h: files[h] = s
sizes = collections.Counter(files.values()); ss = sorted(sizes)
print("PyPI distinct files:", len(files))
for lo, hi, nm in [(0, 1e5, '<100KB'), (1e5, 1e6, '100KB-1MB'), (1e6, 1e7, '1-10MB'), (1e7, 1e9, '>10MB')]:
    fl = [s for s in files.values() if lo <= s < hi]
    if not fl: continue
    u = sum(sizes[s] == 1 for s in fl)
    w = 0
    for s in fl:
        if sizes[s] > 1: continue
        i = bisect.bisect_left(ss, s)
        w += not ((i and s - ss[i-1] <= 300) or (i+1 < len(ss) and ss[i+1] - s <= 300))
    print(f"  {nm}: n={len(fl)} unique={u/len(fl):.3f} unique+-300B={w/len(fl):.3f}")
