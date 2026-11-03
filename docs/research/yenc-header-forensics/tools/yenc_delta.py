import sys, statistics
sys.path.insert(0, '/tmp/claude-0/w4c/scripts')
from concurrent.futures import ProcessPoolExecutor
from yenc_twin import enc, RULES, payload
def f(a):
    kind, seed = a
    d = payload(kind, 716800, seed)
    s = {k: len(enc(d, RULES[k])) for k in ('spec13', 'spec13_brk', 'nyuu', 'nxg')}
    return {k: s[k] - s['spec13'] for k in s}
if __name__ == '__main__':
    with ProcessPoolExecutor() as ex:
        for kind in ('rnd', 'gz'):
            rows = list(ex.map(f, [(kind, s) for s in range(300, 324)]))
            for k in ('spec13_brk', 'nyuu', 'nxg'):
                xs = [r[k] for r in rows]
                print(kind, k, 'delta mean=%.1f sd=%.1f min=%d max=%d' % (statistics.mean(xs), statistics.pstdev(xs), min(xs), max(xs)))
