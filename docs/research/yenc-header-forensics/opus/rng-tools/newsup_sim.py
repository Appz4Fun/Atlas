#!/usr/bin/env python3
"""Faithful-as-possible model of NewsUP's Perl RNG use (Perl>=5.20 internal drand48)
plus an inverter: one own-style Message-ID -> drand48 state -> predict/link the rest."""
import math, random, sys, time

A, C, M = 0x5DEECE66D, 0xB, 1 << 48
AINV = pow(A, -1, M)

class Drand48:
    def __init__(self, seed=None, state=None):
        self.x = state if state is not None else (((seed & 0xFFFFFFFF) << 16) | 0x330E)
    def next(self):
        self.x = (A * self.x + C) % M
        return math.ldexp(float(self.x), -48)
    def rand(self, n=1.0):          # Perl: Drand01() * N
        return self.next() * n

def u_of(x): return math.ldexp(float(x), -48)
def step(x): return (A * x + C) % M
def back(x): return ((x - C) * AINV) % M

ALLOWED = [chr(c) for c in range(48, 58)] + [chr(c) for c in range(65, 91)] + [chr(c) for c in range(97, 123)]
# keys %alpha_chars order is hash-randomized in real Perl; we fix one order (inference: affects only which of -,_,.,$ is chosen)
SET_ALLOWED = ALLOWED + ['-', '_', '.', '$']
ALPHA = set('-_.$')
B36 = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ'

def b36(val):          # Perl: $val % 36 on a NV -> integer modulus of truncated value; int($val/36)
    val = int(val)
    s = ''
    while val:
        s += B36[val % 36]; val //= 36
    return s or '0'

def unb36(s):          # least-significant digit first
    return sum(B36.index(ch) * 36 ** i for i, ch in enumerate(s))

def gen_string(r, length, alphan=False):
    s = ALLOWED[int(r.rand(len(ALLOWED)))]
    length -= 2
    if length < 0: length = 0
    if not alphan:
        prev = ''
        while length:
            length -= 1
            while True:
                cur = SET_ALLOWED[int(r.rand(len(SET_ALLOWED)))]
                if not (cur in ALPHA and prev in ALPHA): break
            s += cur; prev = cur
        s += ALLOWED[int(r.rand(len(ALLOWED)))]
    else:
        while length:
            length -= 1
            s += ALLOWED[int(r.rand(len(ALLOWED)))]
    s += ALLOWED[int(r.rand(len(ALLOWED)))]
    return s

def id_newsup(r, sec, usec):
    st = f"{sec}{usec}"
    return f"{st}.{b36(r.rand(float(st)))}@{b36(int(st))}.newsup"

def id_generic(r, sec, usec):
    return gen_string(r, 8 + int(r.rand(24))) + '@' + gen_string(r, 1 + int(r.rand(12)), True)
def id_gps(r, sec, usec): return f"{sec}.{usec}$gps@gpoststuff"
NM_DOM = ['reader.easyusenet.nl','eu.news.astraweb.com','news.usenet.farm','unliminews.com','reader.usenetbucket.com','news.astraweb.com','reader2.newsxs.nl','reader.usenetdiscounter.com','news.giganews.com','news.usenetserver.com','news.powerusenet.com','news.usenet.net','news.supernews.com','news.rhinonewsgroups.com','news.tweaknews.eu','news.newshosting.com','news.easynews.com','news.frugalusenet.com','news.123usenet.nl','news.alibis.com','post.anarqy.com','news.usenetexpress.com','news.budgetnews.net','east.usenetstorm.com','news.xennews.com','news.yottanews.com','secure.fastusenet.org','news.newsdemon.com']
def id_nm(r, sec, usec): return f"{sec}.{usec}.{int(r.rand(1024))}@{NM_DOM[int(r.rand(28))]}"
def id_nyuu(r, sec, usec): return gen_string(r, 12) + f"-{sec}{usec}@nyuu"
def id_jbinup(r, sec, usec): return gen_string(r, 32) + '@JBinUp.local'
def id_jbindown(r, sec, usec): return gen_string(r, 32) + f".{sec}-{usec}{int(r.rand(9999))}@JBinDown.local"
def id_pp(r, sec, usec):
    d = '@camelsystem-powerpost.local'
    if r.rand(10) > 5: d = '@powerpost2000AA.local'
    a = 1 + int(r.rand(98)); b = 1 + int(r.rand(98))
    return f"part{a}of{b}." + gen_string(r, 22) + d
GENS_OBF = [id_newsup, id_generic, id_gps, id_nm, id_nyuu, id_jbinup, id_jbindown, id_pp]

def uploader(r):
    return (gen_string(r, 8) + ' <' + gen_string(r, int(r.rand(18))) + '@' +
            gen_string(r, 1 + int(r.rand(6))) + '.' + gen_string(r, 1 + int(r.rand(3))) + '>')

def gen_ids(r, n, obf, clock):
    gens = GENS_OBF if obf else [id_newsup]
    out = []
    for _ in range(n):
        g = gens[int(r.rand(len(gens)))]
        sec, usec = clock()
        out.append(g(r, sec, usec))
    return out   # real code returns keys %ids (hash order) -> order shuffled, stream positions unchanged

# ---------------- inverter ----------------
def candidates_from_id(mid):
    """own-style ID -> list of drand48 states X such that int(u(X)*N) == r."""
    left, _ = mid.split('@', 1)
    st, rnd = left.split('.', 1)
    N = float(st); rv = unb36(rnd)
    lo = max(0, math.floor(rv * M / N) - 2); hi = min(M - 1, math.ceil((rv + 1) * M / N) + 2)
    if hi - lo > 5_000_000: return None, st, rv, hi - lo
    c = [x for x in range(lo, hi + 1) if int(u_of(x) * N) == rv]
    return c, st, rv, len(c)

def link(mid_known_state, other_ids, window=20000):
    """Given state of a known own-style ID, check which other own-style IDs sit on the same stream."""
    hits = {}
    fwd = mid_known_state; bwd = mid_known_state
    targets = {}
    for o in other_ids:
        l = o.split('@')[0]; st, rnd = l.split('.', 1)
        targets[o] = (float(st), unb36(rnd))
    for k in range(1, window + 1):
        fwd = step(fwd); bwd = back(bwd)
        for o, (N, rv) in targets.items():
            if o in hits: continue
            if int(u_of(fwd) * N) == rv: hits[o] = +k
            elif int(u_of(bwd) * N) == rv: hits[o] = -k
    return hits

def main():
    trials = int(sys.argv[1]) if len(sys.argv) > 1 else 50
    rng = random.Random(1)
    ok_unique = ok_any = 0; cand_counts = []; link_ok = 0; from_ok = 0; fp = 0
    t0 = time.time()
    for t in range(trials):
        seed = rng.getrandbits(32)
        r = Drand48(seed)
        base = 1_700_000_000 + rng.randrange(10**8)
        tick = [base, rng.randrange(10**6)]
        def clock():
            tick[1] += rng.randrange(1, 300)
            if tick[1] >= 10**6: tick[0] += 1; tick[1] -= 10**6
            return tick[0], tick[1]
        frm = uploader(r)                         # Article->new() with no from -> random uploader
        ids = gen_ids(r, 30, False, clock)
        target = ids[5]
        c, st, rv, n = candidates_from_id(target)
        cand_counts.append(n)
        if c is None: continue
        if c: ok_any += 1
        if len(c) == 1: ok_unique += 1
        # linking: true IDs plus decoys from another seed
        r2 = Drand48(seed ^ 0xdeadbeef)
        decoys = gen_ids(r2, 30, False, clock)
        best = None
        for x in c:
            h = link(x, [i for i in ids if i != target] + decoys, window=200)
            if best is None or len(h) > len(best): best, bx = h, x
        if all(i in best for i in ids if i != target): link_ok += 1
        fp += sum(1 for d in decoys if d in best)
        # predict the From: walk back to the state before uploader (pos 0) and regenerate
        x = bx
        for k in range(200):
            rr = Drand48(state=x);
            if uploader(rr) == frm: from_ok += 1; break
            x = back(x)
    print(f"trials={trials} unique_state={ok_unique} any_state={ok_any} "
          f"cand_count min/max/mean={min(cand_counts)}/{max(cand_counts)}/{sum(cand_counts)/len(cand_counts):.2f} "
          f"all_29_siblings_linked={link_ok} decoy_false_links={fp} From_recovered={from_ok} t={time.time()-t0:.1f}s")
    # demo session
    r = Drand48(12345); tick = [1730000000, 123]
    def clk():
        tick[1] += 137; return tick[0], tick[1]
    print("demo From:", uploader(r)); print("demo IDs:", gen_ids(r, 3, False, clk))
    r = Drand48(12345); print("demo obf IDs:", gen_ids(r, 5, True, clk))
    # short-usec case: usec with few digits makes N small -> more candidates
    r = Drand48(777); sid = id_newsup(r, 1730000000, 5)
    print("short-usec ID", sid, "cands:", candidates_from_id(sid)[3])

if __name__ == '__main__':
    main()
