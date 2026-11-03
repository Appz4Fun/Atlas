# w9a: (V, n, S_last) key collision rate of the w7a RAR5 -hp model on REAL, clustered file-size lists
# Data: Redump DATs mirrored in the libretro-database repo (raw.githubusercontent.com/libretro/libretro-database/master/metadat/redump/<platform>.dat)
#   exact byte sizes + sha1 of disc images (PS1/PS2/PS3/PSP/GC/Wii/X360) -> heavy clustering at media caps (DVD5/DVD9/BD25, fixed GC/Wii images)
# usage: python3 -I w9a_real_sizes.py <dir with .dat files>
import sys, os, re, math, random, collections
sys.path.insert(0, 'docs/research/payload-fingerprints/opus-tools')
from w7a_hp_model import pred
MiB = 2**20
def ngV(U, base=42):  # same approximation as w8a_pitfalls
    mb = U/MiB
    return (int(mb//99)+1 if mb/base > 99 else base)*MiB
romre = re.compile(r'rom \( name "([^"]*)" size (\d+) crc \w+ (?:md5 \w+ )?sha1 (\w+)')
def load(path):
    games = []
    for blk in open(path, encoding='utf-8', errors='replace').read().split('\ngame (')[1:]:
        roms = [(n, int(s), h) for n, s, h in romre.findall(blk)]
        if roms: games.append(roms)
    return games
d = sys.argv[1]
rnd = random.Random(9)
items = []  # (platform, U_big, sha_big, companion sizes)
nmulti = collections.Counter(); ntot = collections.Counter()
for f in sorted(os.listdir(d)):
    if not f.endswith('.dat'): continue
    plat = f[:-4]
    for roms in load(os.path.join(d, f)):
        roms.sort(key=lambda r: -r[1])
        U, _, h = roms[0][0], roms[0][1], roms[0][2]
        tot = sum(r[1] for r in roms)
        if tot < 100e6: continue
        # treat whole game as one payload: big = largest file; companions = other files (cue, extra tracks)
        comp = tuple(sorted(r[1] for r in roms[1:]))
        ntot[plat] += 1; nmulti[plat] += bool(comp)
        items.append((plat, roms[0][1], h, comp))
print('items (largest file >=100MB payloads):', len(items))
for p in ntot: print(f'  {p:32s} N={ntot[p]:6d}  with companion file(s): {nmulti[p]/ntot[p]:.3f}')
def pairs(c): return c*(c-1)//2
def evaluate(pop, Vfun, label, comp=False):
    # pop: list of (U, contentid, compsize or None); L uniform 20-90 per post
    keys = collections.defaultdict(collections.Counter)
    for U, cid, cs in pop:
        L = rnd.randint(20, 90)
        k = (Vfun(U),) + pred(U, Vfun(U), L) + ((cs,) if comp else ())
        keys[k][cid] += 1
    N = len(pop); tot = sum(pairs(sum(c.values())) for c in keys.values())
    same = sum(pairs(v) for c in keys.values() for v in c.values())
    false = tot - same
    p = false/pairs(N)
    ent = -sum(sum(c.values())/N*math.log2(sum(c.values())/N) for c in keys.values())
    print(f'{label:46s} N={N:6d} false pairs={false:8d} p={p:.2e} (-log2 p={-math.log2(p) if p else float("inf"):.1f} bits) H={ent:.2f}b')
    return p
# dedupe by content (sha1 of big file): one entry per distinct image
uniq = {}
for plat, U, h, comp in items: uniq.setdefault(h, (plat, U, comp))
pop = [(U, h, None) for h, (pl, U, c) in uniq.items()]
N = len(pop)
Uc = collections.Counter(U for U, _, _ in pop)
print(f'\ndistinct images N={N}; distinct sizes={len(Uc)}; images sharing exact U with another distinct image: {sum(c for c in Uc.values() if c>1)/N:.3f}')
print('top shared sizes:', [(u, c) for u, c in Uc.most_common(6)])
print('pairs with identical U (irreducible for any size-only key):', sum(pairs(c) for c in Uc.values()), f'p_identU={sum(pairs(c) for c in Uc.values())/pairs(N):.2e}')
print()
res = {}
for lab, Vf in [('ngPost auto V', ngV), ('V=50MB fixed', lambda U: 50*10**6), ('V=100MiB fixed', lambda U: 100*MiB), ('V=42MiB fixed', lambda U: 42*MiB)]:
    res[lab] = evaluate(pop, Vf, 'REAL  ' + lab)
    # same pool excluding exact-U duplicates: clustering effect beyond identical sizes
    seen = collections.Counter(U for U, _, _ in pop)
    evaluate([x for x in pop if seen[x[0]] == 1], Vf, 'REAL  ' + lab + ' (unique-U only)')
    # log-uniform synthetic, same N, same range as w8a
    syn = [(int(math.exp(rnd.uniform(math.log(100e6), math.log(20e9)))), i, None) for i in range(N)]
    evaluate(syn, Vf, 'LOGU  ' + lab)
    # synthetic resampled from real distribution but jittered +-1MB (distinct contents, clustered sizes)
    syn2 = [(U + rnd.randint(-10**6, 10**6), i, None) for i, (U, _, _) in enumerate(pop)]
    evaluate(syn2, Vf, 'REAL+-1MB jitter ' + lab)
# companion (second file exact size) where available
print()
cpop = [(U, h, c) for h, (pl, U, c) in uniq.items() if c]
print(f'distinct images with companion file(s): {len(cpop)}/{N} = {len(cpop)/N:.3f}')
if cpop:
    evaluate([(U, h, None) for U, h, c in cpop], ngV, 'COMP-subset key only, ngPost V')
    evaluate(cpop, ngV, 'COMP-subset key + companion sizes, ngPost V', comp=True)
    evaluate(cpop, lambda U: 100*MiB, 'COMP-subset key + companion sizes, V=100MiB', comp=True)
# --- addendum: libretro DATs keep only one rom per game -> no real companion sizes in this data.
# Modeled companion: exact .nfo size (empirical-like 1-20 KB, uniform) or sample (log-uniform 5-80 MB) attached per distinct image.
print('\n(no real companions in libretro DATs: one rom per game) -> modeled companion sizes:')
nfo = [(U, h, rnd.randint(1000, 20000)) for U, h, _ in pop]
smp = [(U, h, int(math.exp(rnd.uniform(math.log(5e6), math.log(80e6))))) for U, h, _ in pop]
evaluate(nfo, ngV, 'REAL key + modeled nfo size (1-20KB), ngPost V', comp=True)
evaluate(smp, ngV, 'REAL key + modeled sample size (5-80MB), ngPost V', comp=True)
# per-platform: fixed-size image platforms vs variable ones
byp = collections.defaultdict(list)
for h, (pl, U, c) in uniq.items(): byp[pl].append((U, h, None))
print()
for pl, pp in sorted(byp.items()):
    uc = collections.Counter(U for U, _, _ in pp)
    print(f'{pl:30s} shareU={sum(c for c in uc.values() if c>1)/len(pp):.3f}', end='  ')
    evaluate(pp, ngV, 'ngPost V')
