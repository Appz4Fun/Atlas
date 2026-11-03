import sys, random, statistics
sys.path.insert(0, '/tmp/claude-0/w4c/scripts')
from yenc_twin import enc, RULES, payload
n = 716800
d = payload('rnd', n, 7)
for name, rs in RULES.items():
    b1 = enc(d, rs); b2 = enc(bytes(d), rs)
    lines = b1.split(b'\r\n')[:-1]
    print(name, 'deterministic=', b1 == b2, 'maxLineLen=', max(map(len, lines)),
          'lines>128=', sum(len(l) > 128 for l in lines), 'bytes=', len(b1))
# header jitter: XOVER :bytes = header + body; header length varies per post
rnd = random.Random(3)
sizes = []
for s in range(40):
    body = enc(payload('rnd', n, 200 + s), RULES['spec13'])
    hdr = 300 + rnd.randint(0, 120)  # assumed spread of Message-ID/Subject/From/Date lengths
    sizes.append(hdr + len(body))
print('XOVER-like :bytes with assumed 0-120B header jitter sd=', round(statistics.pstdev(sizes), 1))
