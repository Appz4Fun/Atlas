import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
import r5
pw=os.environ.get('PW')
for f in sys.argv[1:]:
    b,hs,end,td=r5.parse(f,pw)
    fh=[h for h in hs if h['type']==2]
    print(os.path.basename(f),len(b),'pk',td,'ovh',len(b)-td,'trail',len(b)-end,'hdrs',[(h['type'],h['hdrlen']) for h in hs],'unp',fh[0].get('unp') if fh else None)
