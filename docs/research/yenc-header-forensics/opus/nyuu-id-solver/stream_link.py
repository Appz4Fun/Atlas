# Wave 8B: stream linking for stock-Nyuu Message-IDs (V8 >= 7.1 Math.random: xorshift128+, state0>>12, 64-entry LIFO cache)
# - forward AND backward stepping of xorshift128+ with the 64-entry cache block structure
# - per-NZB: find ALL streams (repeat solve on still-unlinked IDs), link every ID, per-file stream map
# - cross-NZB: canonical block-boundary anchors; C kernel steps each anchor forward W draws and checks every other anchor
# usage: python3 stream_link.py OUTDIR W nzb1 nzb2 ...
import sys, re, os, time, itertools, subprocess, json
from collections import Counter, defaultdict
M64 = (1 << 64) - 1

# ---------- concrete xorshift128+ ----------
def cstep(s0, s1):
    a = s0; b = s1
    a ^= (a << 23) & M64; a ^= a >> 17; a ^= b; a ^= b >> 26
    return b, a
def _unshr(x, k):
    r = x
    for _ in range(64 // k + 1): r = x ^ (r >> k)
    return r
def _unshl(x, k):
    r = x
    for _ in range(64 // k + 1): r = x ^ ((r << k) & M64)
    return r
def cback(n0, n1):
    b = n0; t = n1 ^ b ^ (b >> 26); t = _unshr(t, 17); a = _unshl(t, 23)
    return a, b
def draw26(s0): return ((s0 >> 12) * 26) >> 52

class Stream:
    """Served Math.random draws. Anchor = state at a cache-block boundary (before a refill).
    Block k (k may be negative) = states anchor+64k+1 .. anchor+64k+64, served LIFO (last generated first).
    Draw index d (relative to anchor) -> block d//64, position d%64 -> generation 64*(d//64)+64-(d%64)."""
    def __init__(self, a, b): self.a, self.b = a, b
    def state_at_block(self, k):
        st = (self.a, self.b)
        if k >= 0:
            for _ in range(64 * k): st = cstep(*st)
        else:
            for _ in range(-64 * k): st = cback(*st)
        return st
    def draws(self, k0, k1):
        """served draw values for blocks k0..k1-1 (concatenated, index 0 = draw 64*k0)"""
        st = self.state_at_block(k0); out = bytearray()
        for _ in range(k1 - k0):
            g = []
            for _ in range(64): st = cstep(*st); g.append(draw26(st[0]))
            out.extend(reversed(g))
        return bytes(out)

# ---------- symbolic GF(2) solver, two IDs with a gap ----------
def _var(off): return [1 << (off + j) for j in range(64)]
def _shl(w, k): return [0] * k + w[:64 - k]
def _shr(w, k): return w[k:] + [0] * k
def _xor(a, b): return [x ^ y for x, y in zip(a, b)]
def _sstep(s0, s1):
    a = s0; b = s1; a = _xor(a, _shl(a, 23)); a = _xor(a, _shr(a, 17)); a = _xor(a, b); a = _xor(a, _shr(b, 26)); return b, a
_S = []
_s0, _s1 = _var(0), _var(64)
for _g in range(320): _s0, _s1 = _sstep(_s0, _s1); _S.append(_s0)
def known_bits(k, N=26):
    lo = -(-k * (1 << 52) // N); hi = ((k + 1) * (1 << 52) - 1) // N; bits = {}
    for j in range(51, -1, -1):
        if (lo >> j) & 1 == (hi >> j) & 1: bits[j] = (lo >> j) & 1
        else: break
    return bits
def gen_of(d):  # 0-based index into _S for draw index d >= 0
    return (d // 64) * 64 + 63 - (d % 64)
def solve(obsA, obsB, gap):
    """obsA at draws o..o+23, obsB at o+gap..o+gap+23. Returns list of (o, a, b) anchors at block 0."""
    out = []
    pos = list(range(24)) + list(range(gap, gap + 24)); obs = obsA + obsB
    for o in range(64):
        piv = {}; ok = True
        for p, v0 in zip(pos, obs):
            for j, v in known_bits(v0).items():
                m = _S[gen_of(p + o)][j + 12]
                while m:
                    h = m.bit_length() - 1
                    if h in piv: pm, pv = piv[h]; m ^= pm; v ^= pv
                    else: piv[h] = (m, v); break
                else:
                    if v: ok = False
                if not ok: break
            if not ok: break
        if not ok or len(piv) < 128: continue
        sol = 0
        for h in sorted(piv):
            m, v = piv[h]; v ^= bin((m & ~(1 << h)) & sol).count('1') & 1
            if v: sol |= 1 << h
        a, b = sol & M64, sol >> 64
        d = Stream(a, b).draws(0, 4)
        if d[o:o + 24] == bytes(obsA) and d[o + gap:o + gap + 24] == bytes(obsB): out.append((o, a, b))
    return out

# ---------- NZB parsing ----------
def parse(path):
    txt = open(path, encoding='utf-8', errors='replace').read(); rows = []
    for fi, fm in enumerate(re.finditer(r'<file\b.*?</file>', txt, re.S)):
        for sm in re.finditer(r'<segment[^>]*number="(\d+)"[^>]*>((?:[A-Z][a-z]){12})-(\d{13})@[^<]*</segment>', fm.group(0)):
            rows.append(dict(file=fi, seg=int(sm.group(1)), pre=sm.group(2), ms=int(sm.group(3))))
    return rows
def dv(pre): return [ord(c) - (65 if j % 2 == 0 else 97) for j, c in enumerate(pre)]

def find_stream(rows, cand_idx, tries=40):
    """solve from time-adjacent pairs among cand_idx; returns (a,b,o,first_row) or None"""
    cand = sorted(cand_idx, key=lambda i: (rows[i]['ms'], i))
    pairs = [(cand[i], cand[i + 1]) for i in range(min(len(cand) - 1, tries))]
    pairs += list(itertools.permutations(cand[:6], 2))
    for p, q in pairs:
        for gap in (48, 24, 72, 96, 144):
            r = solve(dv(rows[p]['pre']), dv(rows[q]['pre']), gap)
            if r:
                o, a, b = r[0]; return a, b, o, p
    return None

def link_all(rows, a, b, margin_blocks):
    """generate +-margin blocks around anchor; index by 24-draw key; return {row_idx: draw_idx_rel_anchor}"""
    S = Stream(a, b); k0 = -margin_blocks; data = S.draws(k0, margin_blocks)
    want = {bytes(dv(r['pre'])): i for i, r in enumerate(rows)}
    first8 = defaultdict(list)
    for key, i in want.items(): first8[key[:8]].append(key)
    res = {}
    for pos in range(len(data) - 24):
        ks = first8.get(data[pos:pos + 8])
        if ks:
            full = data[pos:pos + 24]
            if full in want: res[want[full]] = pos + 64 * k0
    return res

def refine_alignment(rows, left, p, a, b, nsample=40):
    """A wrongly aligned cache block still reproduces IDs that sit inside one block, so the first algebraic
    solution can be off by a few states. Try all 127 shifts of the block boundary and keep the one that links most
    of the time-nearest IDs."""
    near = sorted(left, key=lambda i: abs(rows[i]['ms'] - rows[p]['ms']))[:nsample]
    sub = [rows[i] for i in near]; margin = (nsample * 150) // 64 + 8
    best = (-1, a, b); st0 = (a, b)
    for _ in range(63): st0 = cback(*st0)
    st = st0
    for sh in range(-63, 64):
        n = len(link_all(sub, st[0], st[1], margin))
        if n > best[0]: best = (n, st[0], st[1])
        st = cstep(*st)
    return best[1], best[2]

C_KERNEL = r'''
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
/* argv: W ; stdin: n then n lines "s0 s1" hex. steps each anchor forward W states; reports hits on any other anchor */
int main(int c,char**v){ unsigned long long W=strtoull(v[1],0,10); int n; if(scanf("%d",&n)!=1) return 1;
 uint64_t *A0=malloc(8*n),*A1=malloc(8*n); for(int i=0;i<n;i++) if(scanf("%llx %llx",(unsigned long long*)&A0[i],(unsigned long long*)&A1[i])!=2) return 1;
 /* tiny open-addressing hash on s0 */
 int H=1; while(H<4*n) H<<=1; int *tab=malloc(sizeof(int)*H); for(int i=0;i<H;i++) tab[i]=-1;
 for(int i=0;i<n;i++){ unsigned h=(unsigned)((A0[i]*0x9E3779B97F4A7C15ULL)>>40)&(H-1); while(tab[h]>=0) h=(h+1)&(H-1); tab[h]=i; }
 for(int i=0;i<n;i++){ uint64_t s0=A0[i],s1=A1[i];
  for(unsigned long long t=1;t<=W;t++){ uint64_t a=s0,b=s1; a^=a<<23; a^=a>>17; a^=b; a^=b>>26; s0=b; s1=a;
   unsigned h=(unsigned)((s0*0x9E3779B97F4A7C15ULL)>>40)&(H-1);
   while(tab[h]>=0){ int j=tab[h]; if(A0[j]==s0&&A1[j]==s1&&j!=i) printf("HIT %d %d %llu\n",i,j,t); h=(h+1)&(H-1);} } }
 return 0; }
'''

def main():
    outdir, W = sys.argv[1], int(sys.argv[2]); files = sys.argv[3:]
    os.makedirs(outdir, exist_ok=True)
    anchors = []; report = []
    for f in files:
        rows = parse(f); name = os.path.basename(f)
        if len(rows) < 2: continue
        t = time.time(); left = set(range(len(rows))); streams = []
        while len(left) >= 2 and len(streams) < 8:
            fs = find_stream(rows, left)
            if not fs: break
            a, b, o, p = fs
            a, b = refine_alignment(rows, left, p, a, b)
            margin = (len(rows) * 150) // 64 + 64   # generous: 48-150 draws/article
            L = link_all(rows, a, b, margin)
            if not L: break
            # canonical anchor: block boundary at/below the earliest linked draw
            dmin = min(L.values()); k = dmin // 64
            ca = Stream(a, b).state_at_block(k)
            L = {i: d - 64 * k for i, d in L.items()}
            streams.append(dict(anchor=ca, link=L)); left -= set(L)
        files_streams = defaultdict(Counter)
        for si, s in enumerate(streams):
            for i in s['link']: files_streams[rows[i]['file']][si] += 1
        nfiles = len(set(r['file'] for r in rows))
        info = dict(nzb=name, ids=len(rows), files=nfiles, streams=[], unlinked=len(left), secs=round(time.time() - t, 1))
        for si, s in enumerate(streams):
            L = s['link']; ds = sorted(L.values())
            gaps = Counter(y - x for x, y in zip(ds, ds[1:]))
            ms = [rows[i]['ms'] for i in L]
            # draw order vs ms order agreement
            byd = sorted(L, key=lambda i: L[i]); inv = sum(1 for x, y in zip(byd, byd[1:]) if rows[x]['ms'] > rows[y]['ms'])
            fl = sorted(set(rows[i]['file'] for i in L))
            info['streams'].append(dict(anchor='%016x %016x' % s['anchor'], linked=len(L), span_draws=ds[-1] - ds[0],
                                        files=len(fl), ms_range=[min(ms), max(ms)], gaps=gaps.most_common(6),
                                        ms_order_inversions=inv))
            anchors.append((name, si, s['anchor'], min(ms), max(ms)))
        mixed = sum(1 for fidx, c in files_streams.items() if len(c) > 1)
        info['files_on_multiple_streams'] = mixed
        report.append(info); print(json.dumps(info), flush=True)
    # cross-NZB
    csrc = os.path.join(outdir, 'xs_kernel.c'); cbin = os.path.join(outdir, 'xs_kernel')
    open(csrc, 'w').write(C_KERNEL); subprocess.check_call(['gcc', '-O2', '-o', cbin, csrc])
    inp = '%d\n' % len(anchors) + ''.join('%x %x\n' % a[2] for a in anchors)
    t = time.time(); out = subprocess.run([cbin, str(W)], input=inp, capture_output=True, text=True).stdout
    print('cross-NZB kernel: %d anchors x %d steps in %.1fs' % (len(anchors), W, time.time() - t))
    hits = []
    for line in out.split('\n'):
        if line.startswith('HIT'):
            _, i, j, d = line.split(); i, j, d = int(i), int(j), int(d)
            hits.append(dict(frm=anchors[i][0], frm_stream=anchors[i][1], to=anchors[j][0], to_stream=anchors[j][1], steps=d,
                             dt_ms=anchors[j][3] - anchors[i][3]))
            print('LINK', json.dumps(hits[-1]))
    json.dump(dict(per_nzb=report, anchors=[(a[0], a[1], '%016x %016x' % a[2], a[3], a[4]) for a in anchors], hits=hits, W=W),
              open(os.path.join(outdir, 'stream_link_results.json'), 'w'), indent=1)

if __name__ == '__main__':
    main()
