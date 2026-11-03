import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
import r5
pw=os.environ.get('PW'); tot=0
for f in sys.argv[1:]:
    b,hs,end,td=r5.parse(f,pw)
    fh=[h for h in hs if h['type']==2][0]; q=[h for h in hs if h['type']==3]
    tot+=fh['datasize']
    print(os.path.basename(f),len(b),'filedata',fh['datasize'],fh['datasize']%16,'QOds',[x['datasize'] for x in q],'trailbytes',b[end:].hex()[:40])
print('sum filedata',tot)
