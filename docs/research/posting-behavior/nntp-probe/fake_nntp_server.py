#!/usr/bin/env python3
"""Tiny fake NNTP server for offline testing of nntp_probe.py (plain TCP, localhost).
Usage: python3 -I fake_nntp_server.py PORT [--legacy]
--legacy: OVER/HDR -> 500, LIST COUNTS -> 501 (exercises XOVER/XHDR/LIST ACTIVE fallbacks).
Fails the test loudly (writes VIOLATION to stderr) if it ever receives POST/IHAVE."""
import datetime as dt
import socketserver
import sys

LEGACY = "--legacy" in sys.argv
HOSTILE = "--hostile" in sys.argv  # 201 greeting, 500/502/503 replies, huge NEWNEWS
LOW, HIGH = 1000, 1300
ARTS = {n for n in range(LOW, HIGH + 1) if n % 17 != 0}  # some gaps
USER, PASS = "tester", "s3cret-pw"


def mid(n):
    return "<Part%dof9.ab12CD%05d@powerpost2000AA.local>" % (n % 9, n)


def date(n):
    return "Mon, %02d Nov %s 10:%02d:00 +0000" % (n % 28 + 1, "26" if n % 5 == 0 else "2026", n % 60)


def over_line(n):
    lines = "" if n % 3 == 0 else str(n % 50 + 1)
    return "\t".join([str(n), "subj %d yEnc (1/9)" % n, "poster@example", date(n), mid(n), "",
                      str(700000 + n), lines, "Xref: fake alt.binaries.test:%d" % n])


class H(socketserver.StreamRequestHandler):
    def w(self, s):
        self.wfile.write(s.encode() + b"\r\n")

    def multi(self, first, lines):
        self.w(first)
        for l in lines:
            self.w("." + l if l.startswith(".") else l)
        self.w(".")

    def handle(self):
        authed = False
        user_ok = False
        self.w("201 fake-nntp ready (no posting)" if HOSTILE else "200 fake-nntp ready (posting ok)")
        while True:
            raw = self.rfile.readline()
            if not raw:
                return
            line = raw.decode().rstrip("\r\n")
            p = line.split()
            v = p[0].upper() if p else ""
            if v in ("POST", "IHAVE", "TAKETHIS"):
                sys.stderr.write("VIOLATION: received %s\n" % v)
                self.w("440 no")
                continue
            need_auth = v in ("GROUP", "OVER", "XOVER", "HDR", "XHDR", "NEWNEWS", "HEAD", "STAT", "XZVER", "LIST")
            if need_auth and not authed and not (v == "LIST" and p[1:2] == ["OVERVIEW.FMT"]):
                self.w("480 Authentication required")
                continue
            rng = None
            if len(p) >= 2 and "-" in p[-1] and p[-1].replace("-", "").isdigit():
                a, b = p[-1].split("-")
                rng = [n for n in range(int(a), int(b) + 1) if n in ARTS]
            if HOSTILE and v in ("CAPABILITIES", "DATE", "HELP"):
                self.w("500 What?")
                continue
            if HOSTILE and v in ("HDR", "XHDR"):
                self.w("503 feature not supported")
                continue
            if HOSTILE and v == "NEWNEWS":
                self.w("230 huge list")
                self.wfile.write(b"".join(b"<%d@x>\r\n" % i for i in range(250000)) + b".\r\n")
                continue
            if HOSTILE and v == "STAT":
                self.w("430 no such article")
                continue
            if v == "QUIT":
                self.w("205 bye")
                return
            elif v == "CAPABILITIES":
                caps = ["VERSION 2", "READER", "IMPLEMENTATION FakeNNTP 0.1"]
                if not LEGACY:
                    caps += ["OVER", "HDR", "NEWNEWS", "LIST ACTIVE NEWSGROUPS OVERVIEW.FMT HEADERS COUNTS"]
                caps += ["AUTHINFO USER"] if not authed else []
                self.multi("101 Capability list:", caps)
            elif v == "HELP":
                self.multi("100 help text", ["  XZVER range", "  XFEATURE COMPRESS GZIP", "..dotstuffed line"])
            elif v == "DATE":
                now = dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=3)
                self.w("111 " + now.strftime("%Y%m%d%H%M%S"))
            elif v == "MODE":
                self.w("200 reader mode")
            elif v == "AUTHINFO":
                if p[1].upper() == "USER":
                    user_ok = p[2:] == [USER]
                    self.w("381 more")
                elif p[1].upper() == "PASS":
                    authed = user_ok and line.split(" ", 2)[2] == PASS
                    self.w("281 ok" if authed else "481 bad")
            elif v == "LIST":
                kw = p[1].upper()
                if kw == "OVERVIEW.FMT":
                    self.multi("215 fmt", ["Subject:", "From:", "Date:", "Message-ID:", "References:",
                                           "Bytes:" if LEGACY else ":bytes", "Lines:" if LEGACY else ":lines", "Xref:full"])
                elif kw == "HEADERS":
                    self.multi("215 headers", [":", ":bytes", ":lines"]) if not LEGACY else self.w("500 what")
                elif kw == "COUNTS" and not LEGACY:
                    self.multi("215 counts", ["alt.binaries.test %d %d %d y" % (HIGH, LOW, len(ARTS))])
                elif kw == "ACTIVE":
                    self.multi("215 active", ["alt.binaries.test %d %d y" % (HIGH, LOW)])
                else:
                    self.w("501 syntax")
            elif v == "GROUP":
                self.w("211 %d %d %d %s" % (len(ARTS), LOW, HIGH, p[1]))
            elif v in ("OVER", "HDR") and LEGACY:
                self.w("500 unknown command")
            elif v in ("OVER", "XOVER"):
                self.multi("224 overview", [over_line(n) for n in rng or []])
            elif v in ("HDR", "XHDR"):
                h = p[1].lower()
                vals = {"nntp-posting-date": lambda n: "Mon, 02 Nov 2026 10:00:%02d UTC" % (n % 60),
                        "path": lambda n: "fake!feeder!not-for-mail",
                        "injection-info": lambda n: "fake.local; posting-host=\"x\"; logging-data=\"1\"; mail-complaints-to=\"abuse\"",
                        "x-received-body-crc": lambda n: "(none)" if n % 2 else "1234567890"}.get(h, lambda n: "")
                self.multi("225 hdr" if v == "HDR" else "221 xhdr", ["%d %s" % (n, vals(n)) for n in rng or []])
            elif v == "NEWNEWS":
                self.multi("230 new ids", [mid(n) for n in range(2500)])
            elif v == "HEAD":
                n = int(p[1])
                if n not in ARTS:
                    self.w("423 no such article")
                else:
                    self.multi("221 %d %s" % (n, mid(n)), ["Path: fake!not-for-mail", "From: poster@example",
                                                          "Subject: secret subject", "Message-ID: " + mid(n),
                                                          "X-Folded: a", "\tcontinued", "Date: " + date(n)])
            elif v == "STAT":
                self.w("223 0 %s" % p[1])
            elif v == "XZVER":
                if LEGACY:
                    self.w("500 unknown")
                else:
                    self.w("224 compressed data follows")
                    self.wfile.write(b"=ybegin \x00\x01\xffbinary\x7f\r\nmore\x80\r\n.\r\n")
            elif v == "XFEATURE":
                self.w("290 feature enabled" if not LEGACY else "500 unknown")
            else:
                self.w("500 unknown command")


class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    S(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
