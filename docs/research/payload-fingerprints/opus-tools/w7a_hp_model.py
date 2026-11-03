# w7a: RAR5 `rar a -m0 -hp<pw> -v<V>b -ep1` volume-size model, derived from the header layout (rar 6.23 and 7.12 give identical sizes).
# H(L,ds,U,ts) = encrypted file-header size = 16(IV) + pad16(4(CRC) + vlen(hs) + hs)
#   hs = 68 + L(UTF-8 name bytes) + 2*fw(U) + tsx ; fw(U)=reserved width of packsize & unpsize vints = vlen(U) if U<1MiB else vlen(U<<12)
#        (U in [1MiB,8MiB):5, [8MiB,1GiB):6, [1GiB,128GiB):7 bytes each)
#   tsx = 0 for whole-second mtime (4-byte Unix mtime in base header, fileflags bit 2)
#       = +7 for sub-second mtime (base mtime dropped; HTIME extra rec type 3, flags 0x13 = UNIX|MTIME|NS: 4B sec + 4B ns, 11 bytes) [-tsm1 forces whole-second]
#       = -4 for -ts- (no mtime)
# non-last volume: data D = V - 256 - 2*H exactly (all non-last volumes = V bytes; each holds file hdr + QO(96+H+16) + end hdr)
# last volume:   r = pad16(U) - (n-1)*D ;  S = 126 + H + r + E
#   126 = 8 sig + 38 crypt hdr + 48 main hdr + 32 end hdr
#   E = 0 if r <= QO_T else 112 + H_last   (QO service header 96 + QO data = cached copy of the file header + 16 framing)
import random
QO_T=4096
def vlen(x):
    n=1
    while x>=128: x>>=7; n+=1
    return n
def pad16(x): return -(-x//16)*16
def fw(U):  # width rar reserves for BOTH packsize and unpsize vints (padded vint, 0x80..00): exact below 1 MiB, else vlen(U<<12)
    return vlen(U) if U<2**20 else vlen(U<<12)
def H(L,ds,U,tsx=0):
    hs=68+L+2*fw(U)+tsx
    return 16+pad16(4+vlen(hs)+hs)
def Dnl(V,L,U,tsx=0):  # exact, NOT floored to 16 (encrypted stream pad16(U) is cut anywhere)
    return V-256-2*H(L,0,U,tsx)
def pred(U,V,L,tsx=0):
    D=Dnl(V,L,U,tsx); P=pad16(U); n=max(1,-(-P//D)); r=P-(n-1)*D; h=H(L,r,U,tsx)
    E=(112+h) if r>QO_T else 0
    return n,126+h+r+E
def cands(n,S,V,Ls=range(1,256),tsx=0,Urange=(2**20,2**37)):
    "(L,U) consistent with (n,S) at volume size V: invert last-volume equation per L and width class"
    res=set()
    for L in Ls:
        for w in range(3,8):
            h=16+pad16(4+vlen(68+L+2*w+tsx)+68+L+2*w+tsx)
            for qo in (0,1):
                r=S-126-h-(112+h if qo else 0)
                if r<=0: continue
                D=V-256-2*h; P=(n-1)*D+r
                if P%16: continue
                for U in range(P-15,P+1):
                    if Urange[0]<=U<=Urange[1] and pred(U,V,L,tsx)==(n,S): res.add((L,U))
    return res
# two-file posts (big + small .nfo/.sfv, either order): volume sizes are order-independent.
# Stream = sum over files of (H_i + pad16(U_i)); QO in a volume caches a header copy (H_i+16 bytes) only for file blocks with >4096 data bytes there.
# For a small extra file of size u2<=4096: last S = pred(U+pad16(u2),...)-term shifts by H_2 (measured +H_2=112 for 'x.nfo'), non-last D unchanged
# when the small file sits in the last volume; if it sits in volume 1 (nfo first), volume 1 loses H_2+pad16(u2) of big data, net same totals.
def pred2(U,V,L,u2,L2,tsx=0):
    "big file (U,L) + small extra file (u2,L2), any order, assuming the extra file does not change n"
    n,S=pred(U,V,L,tsx); h2=H(L2,0,u2,tsx)
    return n,S+h2+pad16(u2)+((h2+16) if u2>QO_T else 0)
