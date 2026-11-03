import sys
sys.path.insert(0,'/tmp/claude-0/w6a')
from crc import test
raw = open('/tmp/claude-0/w6a/upd.txt','rb').read().decode()
body = raw.split("\n\n",1)[1]
test("FontOrg update body (fetched 695B)", body, 961135873)
print("body repr:", repr(body))
# also just first line
test("body lines only without 1st/last?", body.rstrip("\n"), 961135873)
