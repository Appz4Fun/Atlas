# RAR5 -hp single-file multivolume: recover unpacked-size candidates from volume sizes only.
# Empirical (rar 6.23, -m0 -hp): every volume carries the same overhead O = 46 + 16k
# (sig8 + crypt hdr38 + enc main/file/QO/end hdrs, multiples of 16; k depends on name length & mtime precision)
# non-last data D = V - O - 18 ; last size = O + roundup16(rem) ; U = (N-1)*D + rem
import sys
def cands(sizes, kmin=20, kmax=60):
    V=sizes[0]; N=len(sizes); S=sizes[-1]; out=[]
    assert all(s==V for s in sizes[:-1]) and S%16==14
    for k in range(kmin,kmax+1):
        O=46+16*k; D=V-O-18; c=S-O
        if c<16 or D<=0: continue
        out.append((k,(N-1)*D+c-15,(N-1)*D+c))
    return out
if __name__=='__main__':
    sizes=[int(x) for x in sys.argv[1:]]
    for k,lo,hi in cands(sizes): print('k=%d O=%d U in [%d,%d]'%(k,46+16*k,lo,hi))
