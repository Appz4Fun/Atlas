# Round 5 task 2: server session keys are per connection (shared queue across hosts/connections), counters per front-end, clock skew.
import random, numpy as np
from collections import defaultdict
def world(P,rng,H=3,F=4):
    skew={(h,f):rng.uniform(-4,4) for h in range(H) for f in range(F)}
    ctr=defaultdict(int); arts=[]
    for p in range(P):
        t0=rng.uniform(0,600); C=rng.randint(6,30); hosts=rng.sample(range(H),rng.randint(1,3))
        asz=rng.choice([716800,768000,512000,750000]); rate=C*rng.uniform(0.5,2.0)  # art/s
        nfiles=rng.randint(10,60); ppf=rng.randint(40,140)
        conns=[]
        for c in range(C):
            h=rng.choice(hosts); f=rng.randrange(F); ctr[(h,f)]+=1
            conns.append([h,f,ctr[(h,f)],rng.randint(200,2000),0])
        t=t0; k=0
        for fi in range(nfiles):
            for pi in range(ppf):
                c=conns[k%C if rng.random()<.7 else rng.randrange(C)]; k+=1
                if c[4]>=c[3]:  # reconnect -> new server session key
                    ctr[(c[0],c[1])]+=1; c[2]=ctr[(c[0],c[1])]; c[4]=0
                c[4]+=1; t+=rng.expovariate(rate)
                arts.append(dict(p=p,key=(c[0],c[1],c[2]),t=int(t+skew[(c[0],c[1])]),sz=asz,subjA=(p,fi),tot=ppf))
    return arts
class UF:
    def __init__(s): s.p={}
    def f(s,x):
        s.p.setdefault(x,x)
        while s.p[x]!=x: s.p[x]=s.p[s.p[x]]; x=s.p[x]
        return x
    def u(s,a,b): s.p[s.f(a)]=s.f(b)
def score(arts,cl):
    tab=defaultdict(int); np_=defaultdict(int); nc=defaultdict(int)
    for a,c in zip(arts,cl): tab[(a['p'],c)]+=1; np_[a['p']]+=1; nc[c]+=1
    tp=sum(v*(v-1)/2 for v in tab.values()); pp=sum(v*(v-1)/2 for v in nc.values()); ap=sum(v*(v-1)/2 for v in np_.values())
    prec=tp/pp if pp else 1; rec=tp/ap
    whole=np.mean([max(tab[(p,c)] for (q,c) in tab if q==p)/np_[p]>=.99 and all(tab[(p,c)]/nc[c]>=.99 for (q,c) in tab if q==p and tab[(p,c)]/np_[p]>=.99) for p in np_])
    return prec,rec,whole
def stitch(arts,useSubj,useTot,tol):
    S=defaultdict(list)
    for i,a in enumerate(arts): S[a['key']].append(i)
    info={k:(min(arts[i]['t'] for i in v),max(arts[i]['t'] for i in v),arts[v[0]]['sz'],{arts[i]['tot'] for i in v}) for k,v in S.items()}
    uf=UF(); ks=list(S)
    for x in range(len(ks)):
        for y in range(x+1,len(ks)):
            a,b=info[ks[x]],info[ks[y]]
            if a[2]!=b[2]: continue
            if useTot and not (a[3]&b[3]): continue
            # time-overlap: consecutive sessions of one connection (reconnect) or parallel sessions of one post
            ov=min(a[1],b[1])-max(a[0],b[0]); span=min(a[1]-a[0],b[1]-b[0])+1
            par= ov>=0.8*span-tol and abs(a[0]-b[0])<=tol+5
            seq= ks[x][:2]==ks[y][:2] and abs(ks[x][2]-ks[y][2])<=3 and (abs(a[1]-b[0])<=tol or abs(b[1]-a[0])<=tol)
            if par or seq: uf.u(ks[x],ks[y])
    if useSubj:
        first={}
        for a in arts:
            s=a['subjA']; 
            if s in first: uf.u(a['key'],first[s])
            else: first[s]=a['key']
    return [uf.f(a['key']) for a in arts]
rng=random.Random(11)
print('P  method                         prec  rec  wholeUploads')
for P in [2,8,16,32]:
    R=defaultdict(list)
    for rep in range(3):
        arts=world(P,rng)
        R['per-post key (old assumption, ideal)'].append((1,1,1))
        R['raw per-conn key']           .append(score(arts,[a['key'] for a in arts]))
        R['overlap+artsize (obf subj)'] .append(score(arts,stitch(arts,False,False,5)))
        R['overlap+artsize+subject/yEnc'].append(score(arts,stitch(arts,True,False,5)))
        R['overlap+artsize+yEnc-total veto'].append(score(arts,stitch(arts,False,True,5)))
        R['subject/yEnc only']          .append(score(arts,stitch(arts,True,False,-1e9)))
    for k,v in R.items(): print(P,f'{k:34s}',' '.join(f'{x:.3f}' for x in np.mean(v,0)),flush=True)
