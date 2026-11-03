import importlib.util, socket, sys, threading, types
spec = importlib.util.spec_from_file_location("p", sys.argv[1]); p = importlib.util.module_from_spec(spec); spec.loader.exec_module(p)
# 1) hard refusals
for bad in ["POST", "post", "IHAVE <a@b>", "TAKETHIS <a@b>", "CHECK <a@b>", "MODE STREAM", "ARTICLE 1", "LIST ACTIVE\r\nPOST", "XFEATURE COMPRESS DEFLATE", "NEWGROUPS 20260101 000000"]:
    try: p.check_read_only(bad); print("NOT REFUSED", bad); sys.exit(1)
    except p.RefusedCommand: pass
print("refusals ok")
assert p.redact("AUTHINFO PASS hunter2") == "AUTHINFO PASS [redacted]"
print(p.msgid_shape("<Ab1.x-9@dom.example>"))
# 2) 480 -> auth -> retry on a raw Conn
a = types.SimpleNamespace(host="127.0.0.1", port=int(sys.argv[2]), tls=False, insecure_tls=False, timeout=5, user="tester")
log=[]; c = p.Conn(a, "s3cret-pw", log, "t"); c.connect()
r = c.cmd("GROUP alt.binaries.test"); print("480 retry:", r)
try: c.send("POST"); print("send allowed POST!"); sys.exit(1)
except p.RefusedCommand as e: print("send refused:", e)
c.close(); print([e["cmd"] for e in log])
# 3) timeout: server that greets then goes silent
srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(1)
def silent():
    s,_ = srv.accept(); s.sendall(b"200 hi\r\n"); import time; time.sleep(4); s.close()
threading.Thread(target=silent, daemon=True).start()
pr = p.Probe(types.SimpleNamespace(host="127.0.0.1", port=srv.getsockname()[1], tls=False, insecure_tls=False, timeout=1.0, user=None,
     group="g", max_lines=5, over_count=200, hdr_count=20, newnews_minutes=10, newnews_cap=2000, no_compress_tests=True, headers=["Path"]), "")
res = pr.run(); print("timeout run errors:", res["errors"][:3], "...", len(res["errors"]))
# 4) overview parsing with a reordered OVERVIEW.FMT, short lines and no-weekday 2-digit dates
pr2 = p.Probe(types.SimpleNamespace(host="x", port=1, tls=False, insecure_tls=False, timeout=1, user=None, group="g",
      max_lines=5, over_count=5, hdr_count=5, newnews_minutes=10, newnews_cap=2000, no_compress_tests=True, headers=[]), "")
pr2.fmt = ["Subject:", "From:", "Date:", "Message-ID:", "References:", "Lines:", "Bytes:"]
pr2.group_info = {"low": 1, "high": 3, "count": 3}
lines = ["1\ts\tf\t2 Nov 26 10:00 GMT\t<a1@d>\t\t\t500", "2\ts\tf\tMon, 2 Nov 2026 10:00:00 +0000\t<b22@d>\t\t7\t600", "3\ts\tf"]
pr2.raw = lambda line, multi=(), keep=None: {"code": 224, "first": "224 ok", "lines": lines, "count": 3, "truncated": False}
o = pr2.do_over()
print("fmt test:", o["lines_field"], o["bytes_present"], o["date_year_digits"], o["msgid_rle_top"])
assert o["lines_field"] == {"empty": 1, "numeric_nonzero": 1, "missing_field": 1} and o["bytes_present"] == 2
assert o["date_year_digits"] == {"2_digit": 1, "4_digit": 1}
print("ALL UNIT TESTS PASSED")
