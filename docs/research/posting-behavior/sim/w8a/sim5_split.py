# Cost to split low-confidence merged clusters: fetch 1 body per session fragment in clusters holding >1 post
# (yEnc name / hash16k of that body names the post), then relabel. Measures bodies per post and resulting whole.
exec(open('sim5_sessions_realistic.py').read().split("rng=random.Random(12)")[0])
rng=random.Random(44)
for P in [8,32]:
    R=[]
    for _ in range(4):
        w=world(P,rng); cl=stitch(w,False,True,2)
        sess=defaultdict(set); posts=defaultdict(set)
        for a,c in zip(w,cl): sess[c].add(a['key']); posts[c].add(a['p'])
        # unknown which clusters are mixed; fetch 1 body per session in clusters spanning >1 front-end-time-window: approximate by fetching all sessions in clusters with >median size
        sizes=sorted(len(s) for s in sess.values()); med=sizes[len(sizes)//2]
        fetch=sum(len(s) for c,s in sess.items() if len(s)>med)
        new=[(c,a['p']) if len(sess[c])>med else c for a,c in zip(w,cl)]
        p,r,wh=score(w,new); R.append((p,r,wh,fetch/P))
    print(P,'after split p r whole bodies/post',' '.join(f'{x:.2f}' for x in np.mean(R,0)),flush=True)
