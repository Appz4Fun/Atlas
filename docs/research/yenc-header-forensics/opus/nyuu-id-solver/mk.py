import random
M64=(1<<64)-1
def cstep(s0,s1):
    a=s0;b=s1;a^=(a<<23)&M64;a^=a>>17;a^=b;a^=b>>26;return b,a
st=(random.getrandbits(64),random.getrandbits(64)); cache=[]
def rnd():
    global st
    if not cache:
        g=[]
        for _ in range(64): st=cstep(*st); g.append(st[0])
        cache.extend(g)  # pop from end => LIFO
    return cache.pop()
for _ in range(random.randrange(64)): rnd()
out=[]
for i in range(40):
    s=''.join(chr((65 if j%2==0 else 97)+(((rnd()>>12)*26)>>52)) for j in range(24))
    out.append('<segment number="%d">%s-%d@nyuu</segment>'%(i+1,s,1672168940000+i))
print('\n'.join(out))
