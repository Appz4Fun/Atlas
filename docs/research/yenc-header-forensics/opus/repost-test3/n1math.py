# Analytic false-hit rate for exact vs n-1 vector match on high-entropy data (segment size sd from synth.py ~90 B at 716800).
import math
for sd in (90,146):
    p=1/(sd*math.sqrt(2)*math.sqrt(2*math.pi))   # P(two independent segment sizes equal)
    print(f'segment sd {sd}: p_eq per segment {p:.4f}')
    for n in (2,3,4,5,6,7,8,10,15):
        ex=p**n; n1=n*p**(n-1)*(1-p)+ex
        need=[N for N in (1e4,1e5,1e6,1e7) if (N*N/2)*n1<0.01]
        print(f'  n={n:2}: exact {ex:.2e}  n-1 {n1:.2e}  n-1 safe (<0.01 expected false pairs) for same-segcount index sizes {need}')
