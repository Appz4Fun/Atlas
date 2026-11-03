#!/usr/bin/env python3
"""Simulate ngPost-style obfuscated uploads spread file-per-group over 4 groups,
and test end-to-start chaining of file runs from XOVER-visible data."""
import heapq, math, random, sys
import numpy as np

ART = 716800
G = 4

def sim_upload(uid, t0, nfiles, N, mbit, groups_mode, rng):
    """Return list of articles: (arrival_t, group, uid, fileidx, part, nparts, size)."""
    files = []
    for f in range(nfiles):
        fsize = rng.randint(50, 300) * ART - rng.randint(1, ART - 1)
        np_ = math.ceil(fsize / ART)
        g = rng.randrange(G) if groups_mode == 'rand' else groups_mode
        files.append((g, np_, fsize))
    queue = []
    for f, (g, np_, fsize) in enumerate(files):
        for p in range(1, np_ + 1):
            sz = ART if p < np_ else fsize - ART * (np_ - 1)
            queue.append((g, f, p, np_, sz))
    per_conn = mbit * 1e6 / N  # fair share
    rtt = rng.uniform(0.01, 0.08)
    conns = [(t0 + rng.uniform(0, 0.2), i) for i in range(N)]
    heapq.heapify(conns)
    out = []
    for (g, f, p, np_, sz) in queue:
        t, i = heapq.heappop(conns)
        dur = rtt + sz * 8 / per_conn * rng.lognormvariate(0, 0.25)
        arr = t + dur
        out.append((arr, g, uid, f, p, np_, sz))
        heapq.heappush(conns, (arr + rtt, i))  # wait 240 then POST again
    return out

def build_world(n_obf, n_bgup, lam, N_choices, seed, horizon=900):
    rng = random.Random(seed)
    arts = []
    meta = {}
    uid = 0
    for k in range(n_obf):
        N = rng.choice(N_choices); mb = rng.uniform(50, 500)
        arts += sim_upload(uid, rng.uniform(0, horizon * 0.5), rng.randint(8, 25), N, mb, 'rand', rng)
        meta[uid] = ('obf', N, mb); uid += 1
    for k in range(n_bgup):  # background non-obfuscated uploads, single group
        N = rng.choice([4, 8, 10, 20, 30, 50]); mb = rng.uniform(20, 500)
        g = rng.randrange(G)
        arts += sim_upload(uid, rng.uniform(0, horizon * 0.8), rng.randint(3, 20), N, mb, g, rng)
        meta[uid] = ('bg', N, mb); uid += 1
    tmax = max(a[0] for a in arts) + 30
    nrng = np.random.default_rng(seed)
    for g in range(G):
        n = nrng.poisson(lam * tmax)
        ts = nrng.uniform(0, tmax, n)
        for t in ts:
            arts.append((float(t), g, -1, -1, 0, 0, 3000))
    # per-group article numbering by arrival order
    bygroup = {g: sorted([a for a in arts if a[1] == g]) for g in range(G)}
    return bygroup, meta, tmax

def hwm_interp(stream, period, tmax, rng):
    """Indexer samples (HWM, wallclock) every `period`s; return interpolated time per artnum."""
    times = np.array([a[0] for a in stream])
    samp_t = np.arange(rng.uniform(0, period), tmax, period)
    hwm = np.searchsorted(times, samp_t)  # count arrived by sample time
    artn = np.arange(len(times)) + 0.5
    return np.interp(artn, hwm, samp_t)

def extract_files(bygroup, meta, tmax, mode, timesrc, period, rng):
    """File runs, keyed via yEnc header (uid/file hidden; indexer sees size+total -> assume exact grouping)."""
    runs = {}
    for g, st in bygroup.items():
        interp = hwm_interp(st, period, tmax, rng)
        secs = np.floor([a[0] for a in st]).astype(int)
        _, first, cnt = np.unique(secs, return_index=True, return_counts=True)
        fi = np.repeat(first, cnt); cc = np.repeat(cnt, cnt)
        rankt = secs + (np.arange(len(st)) - fi + 0.5) / cc
        for n, a in enumerate(st):
            t, _, uid, f, p, np_, sz = a
            if uid < 0: continue
            if mode == 'obf' and meta[uid][0] != 'obf': continue
            if timesrc == 'true': tt = t
            elif timesrc == 'date': tt = math.floor(t) + 0.5
            elif timesrc == 'interp': tt = interp[n]
            elif timesrc == 'rank': tt = rankt[n]
            elif timesrc == 'both':  # interp clipped into Date second
                tt = min(max(interp[n], math.floor(t)), math.floor(t) + 0.999)
            runs.setdefault((uid, f), []).append((n, tt, p, np_, g))
    files = []
    for (uid, f), L in runs.items():
        L.sort()
        ts = np.array([x[1] for x in L])
        parts = np.array([x[2] for x in L])
        np_ = L[0][3]
        # reorder width: max over arrivals of (part - arrival rank)
        rank = np.arange(1, len(parts) + 1)
        width = int(np.max(np.abs(parts - rank)))
        # robust start/end: 2nd and 98th pct of times; rate from middle
        s, e = ts.min(), ts.max()
        dur = max(e - s, 1e-3)
        rate = (len(ts) - 1) / dur
        files.append(dict(uid=uid, f=f, g=L[0][4], s=s, e=e, rate=rate, w=width, n=np_))
    return files

def link(files, lo, hi, rtol, use_w, wtol):
    """Greedy one-to-one successor assignment."""
    by_s = sorted(range(len(files)), key=lambda i: files[i]['s'])
    starts = np.array([files[i]['s'] for i in by_s])
    cand = []
    for i, a in enumerate(files):
        j0 = np.searchsorted(starts, a['e'] + lo); j1 = np.searchsorted(starts, a['e'] + hi)
        for jj in range(j0, j1):
            j = by_s[jj]
            if j == i: continue
            b = files[j]
            if b['s'] <= a['s']: continue
            lr = abs(math.log(b['rate'] / a['rate']))
            if lr > rtol: continue
            if use_w and abs(b['w'] - a['w']) > wtol * max(a['w'], b['w'], 4): continue
            cost = abs(b['s'] - a['e']) / max(hi - lo, 1e-3) + lr / rtol
            cand.append((cost, i, j))
    cand.sort()
    succ, pred = {}, {}
    for c, i, j in cand:
        if i in succ or j in pred: continue
        succ[i] = j; pred[j] = i
    return succ

def score(files, succ):
    truth = set()
    idx = {(x['uid'], x['f']): i for i, x in enumerate(files)}
    for i, x in enumerate(files):
        k = (x['uid'], x['f'] + 1)
        if k in idx: truth.add((i, idx[k]))
    pred = set(succ.items())
    tp = len(pred & truth)
    P = tp / len(pred) if pred else 1.0
    R = tp / len(truth) if truth else 1.0
    # whole-upload exactness: fraction of uploads reconstructed perfectly
    ups = {}
    for i, x in enumerate(files): ups.setdefault(x['uid'], []).append(i)
    perfect = 0
    for u, L in ups.items():
        L.sort(key=lambda i: files[i]['f'])
        ok = all(succ.get(L[k]) == L[k + 1] for k in range(len(L) - 1)) and succ.get(L[-1]) not in [None] + [] and False or \
             all(succ.get(L[k]) == L[k + 1] for k in range(len(L) - 1))
        perfect += ok
    return P, R, perfect / len(ups)

def gaps(files):
    idx = {(x['uid'], x['f']): x for x in files}
    out = []
    for x in files:
        y = idx.get((x['uid'], x['f'] + 1))
        if y: out.append(y['s'] - x['e'])
    return np.array(out)

if __name__ == '__main__':
    exp = sys.argv[1] if len(sys.argv) > 1 else 'main'
    rng = random.Random(1)
    if exp == 'gaps':
        for N in [4, 20, 50]:
            bg, meta, tmax = build_world(5, 0, 20, [N], 7)
            for ts in ['true', 'date', 'interp', 'rank']:
                d = gaps(extract_files(bg, meta, tmax, 'obf', ts, 30, rng))
                print(f'N={N:2d} {ts:6s} gap s(k+1)-e(k): p1={np.percentile(d,1):6.2f} p50={np.median(d):6.2f} p99={np.percentile(d,99):6.2f}')
    if exp == 'width':
        bg, meta, tmax = build_world(30, 0, 20, [4, 10, 20, 50], 3)
        fl = extract_files(bg, meta, tmax, 'obf', 'true', 30, rng)
        by = {}
        for x in fl: by.setdefault(meta[x['uid']][1], []).append(x['w'])
        for N in sorted(by):
            a = np.array(by[N]); print(f'N={N:2d} width: median={np.median(a):.0f} p5={np.percentile(a,5):.0f} p95={np.percentile(a,95):.0f}')
    if exp == 'main':
        lo, hi, rtol = float(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
        for mode, nbg in [('obf', 0), ('all716', 40)]:
            for nob in [1, 5, 20]:
                for lam in [5, 50]:
                    for ts in ['true', 'date', 'rank']:
                        for uw in [0, 1]:
                            Ps, Rs, Us = [], [], []
                            for seed in range(4):
                                bg, meta, tmax = build_world(nob, nbg, lam, [4, 10, 20, 50], 100 + seed)
                                fl = extract_files(bg, meta, tmax, mode, ts, 30, rng)
                                fl = [x for x in fl if x['n'] > 1]
                                P, R, U = score(fl, link(fl, lo, hi, rtol, uw, 0.5))
                                Ps.append(P); Rs.append(R); Us.append(U)
                            print(f'{mode:6s} obf={nob:2d} lam={lam:2d} {ts:5s} W={uw} P={np.mean(Ps):.3f} R={np.mean(Rs):.3f} perfectUp={np.mean(Us):.2f}')
