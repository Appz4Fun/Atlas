# RAR5 -hp -m0 multivolume size model (rar 6.23, measured): non-last data D=V-A(L); last=B(L)+pad16(rem); U=(N-1)D+rem, rem>=1
# measured: L=20 A=512 B=254 ; L=50 A=576 B=286 (V=100000b). A,B step in 16s with L; table below is an APPROX interpolation.
import random
BRK=[18,34,50,65,81,97,113,128]  # measured rar 6.23 & 7.12, V=100000b, second-precision mtime
def AB(L):  # A = per-non-last-volume overhead, B = last-volume overhead (excl. padded rem); B = A/2-2
    a=sum(L>=b for b in BRK)
    if L>128: a=8+(L-128+15)//16  # extrapolated
    return 480+32*a, 238+16*a
def pred(U,V,L):
    A,B=AB(L); D=(V-A)//16*16; n=max(1,-(-U//D)); rem=U-(n-1)*D
    E=240 if rem>4096 else 0  # measured L=20,V=100000; L-dependence of E NOT yet measured (saw 316 at L=110,V=225954)
    return (n, B+E+(-(-rem//16))*16)
def cands(n,S,V,Ls=range(1,256)):
    out=set()
    for L in Ls:
        A,B=AB(L); D=(V-A)//16*16; r=S-B-(240 if S-B>4096+240 else 0)
        for rem in range(max(1,r-15),r+1): out.add((n-1)*D+rem)
    return out
def forward(U,V,L,obs): return pred(U,V,L)==obs
if __name__=='__main__':
    random.seed(1); tot1=tot2=uniq=0; T=200
    for _ in range(T):
        U=random.randint(10**9,8*10**9); L=random.randint(30,90)
        V1,V2=42*2**20,50*2**20
        c1=cands(*pred(U,V1,L),V1); c2=cands(*pred(U,V2,L),V2); I=c1&c2
        assert U in I; tot1+=len(c1); tot2+=len(I); uniq+=len(I)==1
    print('mean |C1|=%.0f mean |C1&C2|=%.2f unique=%d/%d'%(tot1/T,tot2/T,uniq,T))
    # forward check: right release vs wrong ones within +-1MB size or +-1..5 name chars
    random.seed(2); fp=0; n=0
    for _ in range(200):
        U=random.randint(10**9,8*10**9); L=random.randint(30,90); V=50*2**20; obs=pred(U,V,L)
        for dU in random.sample(range(-2**20,2**20),50):
            if dU==0: continue
            n+=1; fp+=forward(U+dU,V,L,obs)
        for dL in (-5,-3,-1,1,3,5):
            n+=1; fp+=forward(U,V,L+dL,obs)
    print('forward-check false positives %d/%d'%(fp,n))
