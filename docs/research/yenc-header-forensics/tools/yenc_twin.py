# yEnc body-identity / size-key experiment. Run: python3 -I yenc_twin.py
import random, gzip, os, sys, statistics, math
from concurrent.futures import ProcessPoolExecutor

RULES = {
    # minimal yEnc 1.3 escaping, rapidyenc-style overflow (escape may push line to 129)
    'spec13':     dict(mode='overflow', crit={0, 10, 13, 61}, edge0=set(), edge_end=set()),
    # strict 128 breaks, minimal escaping (assumed for ngPost / postie)
    'spec13_brk': dict(mode='break',    crit={0, 10, 13, 61}, edge0=set(), edge_end=set()),
    # Nyuu: TAB/SPACE at col0 and line end, '.' at col0
    'nyuu':       dict(mode='break',    crit={0, 10, 13, 61}, edge0={9, 32, 46}, edge_end={9, 32}),
    # nxg-upper: also 0x7F, SPACE at col0/line end, not TAB
    'nxg':        dict(mode='break',    crit={0, 10, 13, 61, 127}, edge0={32}, edge_end={32}),
}

def enc(data, rs, L=128):
    crit, e0, ee, mode = rs['crit'], rs['edge0'], rs['edge_end'], rs['mode']
    out = bytearray(); line = bytearray()
    def tok(o, col0, lastslot):
        if o in crit or (col0 and o in e0) or (lastslot and o in ee):
            return bytes((61, (o + 64) & 255))
        return bytes((o,))
    for b in data:
        o = (b + 42) & 255
        n = len(line)
        t = tok(o, n == 0, n + 1 == L)
        if mode == 'break' and n + len(t) > L:
            out += line + b'\r\n'; line = bytearray()
            t = tok(o, True, False)
        line += t
        if len(line) >= L:
            out += line + b'\r\n'; line = bytearray()
    if line:
        if line[-1] in ee and line[-1] in (9, 32) and len(line) >= 1:
            v = line[-1]; line[-1:] = bytes((61, v + 64))
        out += line + b'\r\n'
    return bytes(out)

def payload(kind, n, seed):
    if kind == 'rnd':
        return random.Random(seed).randbytes(n)
    # gzip stand-in for compressed RAR/MKV: gzip of /usr/share text, distinct per seed
    files = []
    for root, _, fs in os.walk('/usr/share'):
        for f in fs: files.append(os.path.join(root, f))
    files.sort(); random.Random(seed).shuffle(files)
    buf = bytearray(); i = 0
    while len(buf) < n and i < len(files):
        try:
            with open(files[i], 'rb') as fh: chunk = fh.read(400000)
            buf += gzip.compress(chunk, 6, mtime=0)
        except Exception:
            pass
        i += 1
    return bytes(buf[:n])

def stats(xs):
    return statistics.mean(xs), (statistics.pstdev(xs))

def job(args):
    kind, n, seed = args
    d = payload(kind, n, seed)
    res = {}
    for name, rs in RULES.items():
        b = enc(d, rs)
        res[name] = (len(b), b.count(b'\r\n'), b)
    # size-only for the spec13 overflow body is what we vary over many seeds
    return kind, n, seed, {k: (v[0], v[1]) for k, v in res.items()}, \
        {k: hash(v[2]) for k, v in res.items()}

if __name__ == '__main__':
    N = [716800, 768000]
    tasks = [(k, n, s) for k in ('rnd', 'gz') for n in N for s in range(1, 5)]
    with ProcessPoolExecutor() as ex:
        rows = list(ex.map(job, tasks))
    print('== cross-rule comparison (4 seeds per cell) ==')
    for kind in ('rnd', 'gz'):
        for n in N:
            sub = [r for r in rows if r[0] == kind and r[1] == n]
            base = 'spec13'
            for name in RULES:
                same = sum(r[4][name] == r[4][base] for r in sub)
                dsz = [r[3][name][0] - r[3][base][0] for r in sub]
                dl = [r[3][name][1] - r[3][base][1] for r in sub]
                print(f'{kind} {n} {name:11s} bodyIdenticalToSpec13={same}/{len(sub)} '
                      f'meanDeltaBytes={statistics.mean(dsz):+.1f} deltaBytes={dsz} meanDeltaLines={statistics.mean(dl):+.1f}')
    print('== same-rule size variance across payloads (spec13 overflow, 40 seeds) ==')
    for kind in ('rnd', 'gz'):
        for n in N:
            tasks2 = [(kind, n, s) for s in range(100, 140)]
            with ProcessPoolExecutor() as ex:
                r2 = list(ex.map(job, tasks2))
            sz = [r[3]['spec13'][0] for r in r2]
            ln = [r[3]['spec13'][1] for r in r2]
            m, sd = stats(sz)
            distinct = len(set(sz))
            # pairwise equal-size rate
            pairs = [(i, j) for i in range(len(sz)) for j in range(i + 1, len(sz))]
            eq = sum(sz[i] == sz[j] for i, j in pairs) / len(pairs)
            print(f'{kind} {n} bytes mean={m:.1f} sd={sd:.1f} distinct={distinct}/40 '
                  f'pairEqualSizeRate={eq:.4f} normalApprox=1/(2*sqrt(pi)*sd)={1/(2*math.sqrt(math.pi)*sd):.4f} '
                  f'lines mean={statistics.mean(ln):.1f} lines_sd={statistics.pstdev(ln):.2f}')
