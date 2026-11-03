#!/usr/bin/env python3
"""nntp_probe.py -- strictly READ-ONLY NNTP capability probe (Python 3 stdlib only).

Records which reader commands a provider supports and what shape its overview /
header data has, for Atlas linking research. It never posts: the send function
only allows a fixed list of read verbs and refuses POST, IHAVE, TAKETHIS, CHECK,
MODE STREAM and everything else.

Password comes ONLY from the NNTP_PASS environment variable. It is never taken
from argv, printed, logged or written to the output file. AUTHINFO lines are
redacted in the command log. Subjects, From and article bodies are never stored;
Message-IDs are stored only as a shape (local part -> character classes).

Usage:
  NNTP_PASS='...' python3 -I nntp_probe.py --host news.example.com --user me \
      --group alt.binaries.test --out results.json
"""
import argparse
import collections
import datetime as dt
import json
import os
import re
import socket
import ssl
import statistics
import sys
import time

VERSION = "1.0"
MAXLINE = 1 << 20

# Only these verbs can ever leave this program. Everything else is refused.
ALLOWED_VERBS = {
    "CAPABILITIES", "HELP", "DATE", "MODE", "LIST", "GROUP", "OVER", "XOVER",
    "HDR", "XHDR", "NEWNEWS", "HEAD", "STAT", "XZVER", "XFEATURE", "AUTHINFO", "QUIT",
}
FORBIDDEN_VERBS = {"POST", "IHAVE", "TAKETHIS", "CHECK", "XREPLIC", "ARTICLE", "BODY"}
ALLOWED_LIST_KW = {"OVERVIEW.FMT", "HEADERS", "COUNTS", "ACTIVE"}


class RefusedCommand(Exception):
    pass


def check_read_only(line):
    """Hard gate: raise RefusedCommand unless `line` is a known read-only command."""
    if "\r" in line or "\n" in line or "\0" in line:
        raise RefusedCommand("control characters in command")
    parts = line.split()
    if not parts:
        raise RefusedCommand("empty command")
    verb = parts[0].upper()
    if verb in FORBIDDEN_VERBS or verb not in ALLOWED_VERBS:
        raise RefusedCommand("verb %r is not allowed (read-only probe)" % verb)
    if verb == "MODE" and [p.upper() for p in parts[1:]] != ["READER"]:
        raise RefusedCommand("only MODE READER is allowed")
    if verb == "LIST" and (len(parts) < 2 or parts[1].upper() not in ALLOWED_LIST_KW):
        raise RefusedCommand("LIST keyword not allowed")
    if verb == "XFEATURE" and [p.upper() for p in parts[1:]] != ["COMPRESS", "GZIP"]:
        raise RefusedCommand("only XFEATURE COMPRESS GZIP is allowed")
    if verb == "AUTHINFO" and (len(parts) < 2 or parts[1].upper() not in ("USER", "PASS")):
        raise RefusedCommand("only AUTHINFO USER/PASS allowed")
    return verb


def redact(line):
    if line.upper().startswith("AUTHINFO"):
        p = line.split()
        return " ".join(p[:2]) + " [redacted]"
    return line


def utcnow():
    return dt.datetime.now(dt.timezone.utc)


# ---------------------------------------------------------------- shapes

def char_shape(s):
    out = []
    for ch in s:
        if ch.isascii() and ch.isalpha():
            out.append("A")
        elif ch.isdigit():
            out.append("d")
        else:
            out.append(ch if ch.isascii() and ch.isprintable() else "?")
    return "".join(out)


def rle(shape):
    out = []
    for m in re.finditer(r"(.)\1*", shape):
        n = len(m.group(0))
        out.append(m.group(1) + ("{%d}" % n if n > 1 else ""))
    return "".join(out)


def msgid_shape(mid):
    """'<abc123@x.com>' -> {'shape': 'AAAddd@x.com', 'rle': 'A{3}d{3}@x.com'}."""
    m = mid.strip()
    if m.startswith("<") and m.endswith(">"):
        m = m[1:-1]
    local, _, domain = m.rpartition("@")
    if not _:
        local, domain = m, ""
    s = char_shape(local)
    return {"shape": s + ("@" + domain if domain else ""),
            "rle": rle(s) + ("@" + domain if domain else ""),
            "local_len": len(local), "domain": domain}


def text_shape(s):
    return re.sub(r"[A-Za-z]", "a", re.sub(r"\d", "d", s))


# ---------------------------------------------------------------- connection

class Conn:
    def __init__(self, args, password, log, label):
        self.a = args
        self.password = password
        self.log = log
        self.label = label
        self.sock = None
        self.f = None
        self.authed = False
        self.auth_result = None
        self.t0 = time.monotonic()

    def connect(self):
        raw = socket.create_connection((self.a.host, self.a.port), timeout=self.a.timeout)
        if self.a.tls:
            ctx = ssl.create_default_context()
            if self.a.insecure_tls:
                ctx.check_hostname = False
                ctx.verify_mode = ssl.CERT_NONE
            self.sock = ctx.wrap_socket(raw, server_hostname=self.a.host)
        else:
            self.sock = raw
        self.sock.settimeout(self.a.timeout)
        self.f = self.sock.makefile("rb")
        first = self._readline()
        self._log("<connect>", first)
        return first

    def close(self, quit_=True):
        try:
            if quit_ and self.sock:
                self.send("QUIT")
                self.sock.settimeout(3)
                self.f.readline(MAXLINE)
        except Exception:
            pass
        for o in (self.f, self.sock):
            try:
                o.close()
            except Exception:
                pass
        self.sock = self.f = None

    def _log(self, cmd, first):
        self.log.append({"conn": self.label, "t": round(time.monotonic() - self.t0, 3),
                         "cmd": redact(cmd), "first": first[:300]})

    def _readline(self):
        b = self.f.readline(MAXLINE)
        if not b:
            raise ConnectionError("server closed the connection")
        if b.endswith(b"\r\n"):
            b = b[:-2]
        elif b.endswith(b"\n"):
            b = b[:-1]
        return b.decode("utf-8", "replace")

    def send(self, line):
        check_read_only(line)  # hard gate, raises RefusedCommand
        self.sock.sendall(line.encode("utf-8") + b"\r\n")

    def read_multi(self, keep, hard_cap):
        lines, n, truncated = [], 0, False
        while True:
            ln = self._readline()
            if ln == ".":
                return lines, n, truncated
            if ln.startswith(".."):
                ln = ln[1:]  # dot-unstuffing
            n += 1
            if n <= keep:
                lines.append(ln)
            else:
                truncated = True
            if n >= hard_cap:
                raise OverflowError("response exceeded hard cap of %d lines" % hard_cap)

    def drain_binary(self, max_bytes=4 << 20):
        """Discard a (possibly binary, e.g. XZVER) multi-line body; return byte count."""
        total = 0
        while True:
            b = self.f.readline(MAXLINE)
            if not b:
                raise ConnectionError("closed while draining")
            total += len(b)
            if b == b".\r\n" or b == b".\n":
                return total
            if total > max_bytes:
                raise OverflowError("binary body too large; connection abandoned")

    def auth(self):
        if not self.a.user:
            return None
        res = {"user_code": None, "pass_code": None}
        self.send("AUTHINFO USER " + self.a.user)
        first = self._readline()
        self._log("AUTHINFO USER", first)
        res["user_code"] = code_of(first)
        if res["user_code"] == 381:
            if not self.password:
                res["error"] = "server wants a password but NNTP_PASS is empty"
                self.auth_result = res
                return res
            self.send("AUTHINFO PASS " + self.password)
            first = self._readline()
            self._log("AUTHINFO PASS", first)
            res["pass_code"] = code_of(first)
        self.authed = res["user_code"] == 281 or res["pass_code"] == 281
        res["ok"] = self.authed
        self.auth_result = res
        return res

    def cmd(self, line, multi=(), keep=40, hard_cap=200000, _retry=True):
        """Send one command; return a dict with code/first/lines/count/truncated."""
        self.send(line)
        first = self._readline()
        self._log(line, first)
        code = code_of(first)
        if code == 480 and _retry and self.a.user and self.auth_result is None:
            # authenticate at most once per connection (avoid lockouts on bad creds)
            self.auth()
            if self.authed:
                r = self.cmd(line, multi, keep, hard_cap, _retry=False)
                r["after_480_auth"] = True
                return r
        r = {"code": code, "first": first}
        if code in multi:
            lines, n, tr = self.read_multi(keep, hard_cap)
            r.update(lines=lines, count=n, truncated=tr)
        return r


def code_of(first):
    try:
        return int(first[:3])
    except ValueError:
        return None


# ---------------------------------------------------------------- probe

class Probe:
    def __init__(self, args, password):
        self.a = args
        self.pw = password
        self.log = []
        self.res = {"meta": {
            "probe_version": VERSION, "host": args.host, "port": args.port, "tls": args.tls,
            "user_supplied": bool(args.user), "group": args.group,
            "started_utc": utcnow().isoformat(timespec="seconds"),
            "python": sys.version.split()[0]}, "steps": {}, "errors": []}
        self.c = None
        self.server_now = None
        self.fmt = None
        self.over_ids = []
        self.over_nums = []
        self.group_info = None

    # -- helpers
    def new_conn(self, label):
        c = Conn(self.a, self.pw, self.log, label)
        greet = c.connect()
        return c, greet

    def ensure(self):
        if self.c is None or self.c.sock is None:
            recs = self.res["steps"].setdefault("reconnects", [])
            if len(recs) >= 3:
                raise ConnectionError("gave up after 3 reconnects")
            auth_bad = bool(self.a.user) and _dig(self.res["steps"], "auth", "ok") is False
            recs.append("pending")
            self.c, greet = self.new_conn("main-reconnect")
            recs[-1] = greet[:200]
            if self.a.user and not auth_bad:
                self.c.auth()
            self.c.cmd("MODE READER")
            if self.group_info:
                self.c.cmd("GROUP " + self.a.group)

    def step(self, name, fn):
        t = time.monotonic()
        try:
            self.ensure()
            out = fn()
        except RefusedCommand as e:
            out = {"error": "refused locally: %s" % e}
        except (socket.timeout, TimeoutError) as e:
            out = {"error": "timeout: %s" % e}
            self._drop()
        except (OSError, ConnectionError, OverflowError, ssl.SSLError) as e:
            out = {"error": "%s: %s" % (type(e).__name__, e)}
            self._drop()
        except Exception as e:  # keep going no matter what
            out = {"error": "unexpected %s: %s" % (type(e).__name__, e)}
            self._drop()
        if isinstance(out, dict):
            out.setdefault("elapsed_s", round(time.monotonic() - t, 3))
            if "error" in out:
                self.res["errors"].append({"step": name, "error": out["error"]})
        self.res["steps"][name] = out
        return out

    def _drop(self):
        if self.c:
            self.c.close(quit_=False)
        self.c = None

    def raw(self, line, multi=(), keep=None):
        r = self.c.cmd(line, multi, keep=self.a.max_lines if keep is None else keep)
        return r

    # -- run
    def run(self):
        a = self.a
        try:
            self.c, greet = self.new_conn("main")
        except Exception as e:
            self.res["fatal"] = "connect failed: %s: %s" % (type(e).__name__, e)
            return self.res
        self.res["steps"]["greeting"] = {"first": greet, "code": code_of(greet),
                                         "posting_allowed": code_of(greet) == 200}
        if code_of(greet) not in (200, 201):
            self.res["fatal"] = "server refused service"
            self.c.close()
            return self.res

        self.step("capabilities_pre_auth", lambda: self.raw("CAPABILITIES", (101,)))
        # MODE READER before AUTHINFO: mode-switching servers (e.g. INN) need it first
        self.step("mode_reader", lambda: self.raw("MODE READER"))
        if a.user:
            self.step("auth", lambda: (self.c.auth_result if self.c.auth_result is not None
                                       else self.c.auth()) or {"skipped": True})
        else:
            self.res["steps"]["auth"] = {"skipped": "no --user given"}
        self.step("capabilities", lambda: self.raw("CAPABILITIES", (101,)))
        self.step("help", lambda: self.raw("HELP", (100,)))
        self.step("date", self.do_date)
        self.step("list_overview_fmt", self.do_fmt)
        self.step("list_headers", lambda: self.raw("LIST HEADERS", (215,)))
        self.step("list_counts", self.do_counts)
        self.step("group", self.do_group)
        if self.group_info:
            self.step("over", self.do_over)
            self.step("hdr", self.do_hdr)
            self.step("newnews", self.do_newnews)
            self.step("head", self.do_head)
            self.step("stat", self.do_stat)
            if not a.no_compress_tests:
                self.step("xzver", self.do_xzver)
        if self.c:
            self.c.close()
            self.c = None
        auth_failed = bool(a.user) and not _dig(self.res["steps"], "auth", "ok")
        if auth_failed:
            self.res["steps"]["xfeature_compress_gzip"] = {"skipped": "main auth failed; no second login"}
        elif not a.no_compress_tests:
            self.res["steps"]["xfeature_compress_gzip"] = self.do_xfeature()
        self.res["meta"]["finished_utc"] = utcnow().isoformat(timespec="seconds")
        self.res["command_log"] = self.log
        self.res["summary"] = self.summary()
        return self.res

    # -- individual steps
    def do_date(self):
        r = self._date_once()
        skews = [r.get("skew_s_server_minus_local")]
        for _ in range(2):  # extra samples: server DATE has 1 s resolution
            skews.append(self._date_once().get("skew_s_server_minus_local"))
        skews = [x for x in skews if x is not None]
        if skews:
            r["skew_samples_s"] = skews
            r["skew_s_server_minus_local"] = round(statistics.median(skews), 3)
        return r

    def _date_once(self):
        before = utcnow()
        r = self.raw("DATE")
        after = utcnow()
        local_mid = before + (after - before) / 2
        r["local_utc_mid"] = local_mid.isoformat(timespec="milliseconds")
        r["rtt_ms"] = round((after - before).total_seconds() * 1000, 1)
        m = re.match(r"111\s+(\d{14})", r["first"])
        if m:
            sd = dt.datetime.strptime(m.group(1), "%Y%m%d%H%M%S").replace(tzinfo=dt.timezone.utc)
            self.server_now = (sd, local_mid)
            r["server_utc"] = sd.isoformat()
            # server has 1 s resolution: skew is accurate to about +-1 s + rtt/2
            r["skew_s_server_minus_local"] = round((sd - local_mid).total_seconds(), 3)
        return r

    def do_fmt(self):
        r = self.raw("LIST OVERVIEW.FMT", (215,), keep=200)
        if r["code"] == 215:
            self.fmt = [l.strip() for l in r["lines"] if l.strip()]
        return r

    def do_counts(self):
        r = self.raw("LIST COUNTS " + self.a.group, (215,))
        out = {"list_counts": r}
        if r["code"] != 215:
            out["list_active"] = self.raw("LIST ACTIVE " + self.a.group, (215,))
        return out

    def do_group(self):
        r = self.raw("GROUP " + self.a.group)
        m = re.match(r"211\s+(\d+)\s+(\d+)\s+(\d+)", r["first"])
        if m:
            cnt, lo, hi = map(int, m.groups())
            self.group_info = {"count": cnt, "low": lo, "high": hi}
            r.update(self.group_info)
        return r

    def fmt_index(self):
        fmt = self.fmt or ["Subject:", "From:", "Date:", "Message-ID:", "References:",
                           ":bytes", ":lines", "Xref:full"]
        names = []
        for f in fmt:
            n = f.lower().replace(":full", "").strip()
            n = n.strip(":")
            names.append(n)
        idx = {}
        for i, n in enumerate(names):
            idx.setdefault(n, i)
        return idx, fmt

    def do_over(self):
        hi = self.group_info["high"]
        lo = max(self.group_info["low"], hi - self.a.over_count + 1)
        rng = "%d-%d" % (lo, hi)
        out = {"range": rng}
        r = self.raw("OVER " + rng, (224,), keep=10 ** 6)
        out["over"] = {k: v for k, v in r.items() if k != "lines"}
        lines = r.get("lines")
        if r["code"] != 224:
            r2 = self.raw("XOVER " + rng, (224,), keep=10 ** 6)
            out["xover"] = {k: v for k, v in r2.items() if k != "lines"}
            lines = r2.get("lines")
        out["verb_used"] = "OVER" if r["code"] == 224 else ("XOVER" if lines is not None else None)
        if lines is None:
            return out
        idx, fmt = self.fmt_index()
        out["fmt_used"] = fmt
        li = idx.get("lines")
        bi = idx.get("bytes")
        di = idx.get("date")
        mi = idx.get("message-id")
        lines_stats = collections.Counter()
        bytes_vals, years, date_shapes, ids = [], collections.Counter(), collections.Counter(), []
        shapes, domains, rles = collections.Counter(), collections.Counter(), collections.Counter()
        nfields = collections.Counter()
        for ln in lines:
            p = ln.split("\t")
            nfields[len(p)] += 1
            try:
                self.over_nums.append(int(p[0]))
            except ValueError:
                pass
            fld = p[1:]

            def get(i):
                return fld[i].strip() if i is not None and i < len(fld) else None
            lv = get(li)
            if lv is None:
                lines_stats["missing_field"] += 1
            elif lv == "":
                lines_stats["empty"] += 1
            elif lv == "0":
                lines_stats["zero"] += 1
            elif lv.isdigit():
                lines_stats["numeric_nonzero"] += 1
            else:
                lines_stats["other"] += 1
            bv = get(bi)
            if bv and bv.isdigit():
                bytes_vals.append(int(bv))
            dv = get(di)
            if dv:
                date_shapes[text_shape(dv)] += 1
                m = re.search(r"\b\d{1,2}\s+[A-Za-z]{3}\s+(\d+)\b", dv)
                years["%d_digit" % len(m.group(1)) if m else "unparsed"] += 1
            mv = get(mi)
            if mv:
                ids.append(mv)
                s = msgid_shape(mv)
                shapes[s["shape"]] += 1
                rles[s["rle"]] += 1
                domains[s["domain"]] += 1
        self.over_ids = ids
        nums = sorted(set(self.over_nums))
        out.update({
            "articles_returned": len(lines),
            "articles_requested": hi - lo + 1,
            "fields_per_line": dict(nfields),
            "lines_field": dict(lines_stats),
            "lines_field_all_empty_or_zero": bool(lines) and
                lines_stats["empty"] + lines_stats["zero"] + lines_stats["missing_field"] == len(lines),
            "bytes_present": len(bytes_vals),
            "bytes_min": min(bytes_vals) if bytes_vals else None,
            "bytes_median": statistics.median(bytes_vals) if bytes_vals else None,
            "bytes_max": max(bytes_vals) if bytes_vals else None,
            "date_year_digits": dict(years),
            "date_shapes_top": date_shapes.most_common(5),
            "msgid_domains_top": domains.most_common(10),
            "msgid_shapes_top": shapes.most_common(10),
            "msgid_rle_top": rles.most_common(10),
            "msgid_distinct_shapes": len(shapes),
            "article_number_gaps": (nums[-1] - nums[0] + 1 - len(nums)) if nums else None,
        })
        return out

    def do_hdr(self):
        hi = self.group_info["high"]
        lo = max(self.group_info["low"], hi - self.a.hdr_count + 1)
        rng = "%d-%d" % (lo, hi)
        out = {"range": rng}
        for h in self.a.headers:
            res = {}
            r = self.raw("HDR %s %s" % (h, rng), (225,), keep=10 ** 5)
            res["hdr_code"] = r["code"]
            lines = r.get("lines")
            verb = "HDR"
            if r["code"] != 225:
                r2 = self.raw("XHDR %s %s" % (h, rng), (221,), keep=10 ** 5)
                res["xhdr_code"] = r2["code"]
                res["xhdr_first"] = r2["first"][:120]
                lines, verb = r2.get("lines"), "XHDR"
            res["hdr_first"] = r["first"][:120]
            if lines is not None:
                vals = []
                for ln in lines:
                    _, _, v = ln.partition(" ")
                    v = v.strip()
                    vals.append("" if v in ("", "(none)") else v)
                have = [v for v in vals if v]
                res.update(verb_used=verb, lines=len(vals), with_value=len(have))
                hl = h.lower()
                if hl == "nntp-posting-date":
                    res["value_shapes"] = collections.Counter(text_shape(v) for v in have).most_common(3)
                elif hl == "path":
                    if have:
                        res["path_elements_median"] = statistics.median(len(v.split("!")) for v in have)
                elif hl == "injection-info":
                    keys = collections.Counter()
                    for v in have:
                        for k in re.findall(r";\s*([A-Za-z-]+)\s*=", v):
                            keys[k.lower()] += 1
                    res["param_names"] = keys.most_common(10)
                elif hl == "x-received-body-crc":
                    res["value_shapes"] = collections.Counter(text_shape(v) for v in have).most_common(3)
            out[h] = res
        return out

    def do_newnews(self):
        if self.server_now:
            sd, local_mid = self.server_now
            base, src = sd + (utcnow() - local_mid), "server_clock"
        else:
            base, src = utcnow(), "local_clock"
        since = base - dt.timedelta(minutes=self.a.newnews_minutes)
        line = "NEWNEWS %s %s GMT" % (self.a.group, since.strftime("%Y%m%d %H%M%S"))
        t = time.monotonic()
        try:
            r = self.raw(line, (230,), keep=self.a.newnews_cap)
        except OverflowError:
            self._drop()
            return {"code": 230, "count": ">=200000", "truncated": True, "since_utc": since.isoformat(timespec="seconds"),
                    "seconds": round(time.monotonic() - t, 3), "note": "list too long; connection dropped and reopened",
                    "minutes": self.a.newnews_minutes}
        r["seconds"] = round(time.monotonic() - t, 3)
        r["since_utc"] = since.isoformat(timespec="seconds")
        r["since_source"] = src
        r["minutes"] = self.a.newnews_minutes
        ids = r.pop("lines", None)
        if ids is not None:
            r["stored_ids"] = len(ids)
            r["shapes_top"] = collections.Counter(msgid_shape(i)["rle"] for i in ids).most_common(5)
            over = set(self.over_ids)
            r["overlap_with_over_sample"] = sum(1 for i in ids if i.strip() in over)
        return r

    def do_head(self):
        nums = sorted(set(self.over_nums))[-3:] or [self.group_info["high"] - i for i in range(3)]
        out = {"articles": []}
        union = collections.Counter()
        for n in nums:
            r = self.raw("HEAD %d" % n, (221,), keep=2000)
            names = []
            for ln in r.get("lines", []):
                if ln[:1] in (" ", "\t"):
                    continue  # folded continuation
                nm = ln.split(":", 1)[0]
                if nm and ":" in ln:
                    names.append(nm)
            union.update(set(names))
            out["articles"].append({"code": r["code"], "header_names": names})
        out["header_name_frequency"] = sorted(union.items())
        return out

    def do_stat(self):
        ids = self.over_ids[-3:]
        out = {"results": []}
        if not ids:
            out["skipped"] = "no message-ids from OVER"
            return out
        for mid in ids:
            mid = mid.strip()
            if not (mid.startswith("<") and mid.endswith(">")) or " " in mid:
                out["results"].append({"id_shape": msgid_shape(mid)["rle"], "skipped": "malformed id"})
                continue
            r = self.raw("STAT " + mid)
            out["results"].append({"id_shape": msgid_shape(mid)["rle"], "code": r["code"]})
        return out

    def do_xzver(self):
        hi = self.group_info["high"]
        line = "XZVER %d-%d" % (max(self.group_info["low"], hi - 1), hi)
        self.c.send(line)
        first = self.c._readline()
        self.c._log(line, first)
        code = code_of(first)
        out = {"code": code, "first": first[:120]}
        if code is not None and 200 <= code < 300:
            out["drained_bytes"] = self.c.drain_binary()
        return out

    def do_xfeature(self):
        """Separate throwaway connection: XFEATURE COMPRESS GZIP turns on
        compression for the rest of the session, so we only send it and QUIT."""
        out = {}
        t = time.monotonic()
        c = None
        try:
            c, greet = self.new_conn("xfeature")
            if self.a.user:
                c.auth()
            r = c.cmd("XFEATURE COMPRESS GZIP")
            out.update(code=r["code"], first=r["first"][:120])
        except Exception as e:
            out["error"] = "%s: %s" % (type(e).__name__, e)
            self.res["errors"].append({"step": "xfeature_compress_gzip", "error": out["error"]})
        finally:
            if c:
                c.close()  # QUIT reply may be gzip; ignored
        out["elapsed_s"] = round(time.monotonic() - t, 3)
        return out

    # -- summary
    def summary(self):
        s = self.res["steps"]
        g = lambda *ks: _dig(s, *ks)
        caps = g("capabilities", "lines") or []
        caps_u = [c.upper() for c in caps]
        help_u = " ".join(g("help", "lines") or []).upper()
        hdr = s.get("hdr", {}) if isinstance(s.get("hdr"), dict) else {}
        rows = [
            ("greeting", g("greeting", "first")),
            ("auth", "skipped" if not self.a.user else
             ("ok" if g("auth", "ok") else "FAILED %s/%s" % (g("auth", "user_code"), g("auth", "pass_code")))),
            ("CAPABILITIES", g("capabilities", "code")),
            ("caps: OVER / HDR / NEWNEWS",
             "%s / %s / %s" % (any(c.startswith("OVER") for c in caps_u),
                               any(c.startswith("HDR") for c in caps_u),
                               any(c.startswith("NEWNEWS") for c in caps_u))),
            ("caps: IMPLEMENTATION", next((c for c in caps if c.upper().startswith("IMPLEMENTATION")), None)),
            ("HELP mentions XZVER / XFEATURE", "%s / %s" % ("XZVER" in help_u, "XFEATURE" in help_u)),
            ("MODE READER", g("mode_reader", "code")),
            ("DATE skew s (server-local)", g("date", "skew_s_server_minus_local")),
            ("LIST OVERVIEW.FMT", "%s %s" % (g("list_overview_fmt", "code"),
                                              ",".join(g("list_overview_fmt", "lines") or []))),
            ("LIST HEADERS", "%s (%s lines)" % (g("list_headers", "code"), g("list_headers", "count"))),
            ("LIST COUNTS / ACTIVE", "%s / %s" % (g("list_counts", "list_counts", "code"),
                                                   g("list_counts", "list_active", "code"))),
            ("GROUP", g("group", "first")),
            ("OVER verb used", g("over", "verb_used")),
            ("OVER returned/requested", "%s/%s" % (g("over", "articles_returned"), g("over", "articles_requested"))),
            ("Lines field", g("over", "lines_field")),
            ("Lines all empty/zero", g("over", "lines_field_all_empty_or_zero")),
            ("Date year digits", g("over", "date_year_digits")),
            ("Msg-ID top shape", (g("over", "msgid_rle_top") or [[None]])[0][0]),
            ("Msg-ID distinct shapes", g("over", "msgid_distinct_shapes")),
        ]
        for h in self.a.headers:
            r = hdr.get(h) or {}
            if not r:
                rows.append(("HDR %s" % h, None))
                continue
            codes = "HDR %s" % r.get("hdr_code") + (" XHDR %s" % r["xhdr_code"] if "xhdr_code" in r else "")
            got = ("%s/%s with value via %s" % (r.get("with_value"), r.get("lines"), r.get("verb_used"))
                   if r.get("verb_used") else "not available")
            rows.append(("HDR %s" % h, "%s: %s" % (codes, got)))
        rows += [
            ("NEWNEWS code / count / truncated", "%s / %s / %s" % (
                g("newnews", "code"), g("newnews", "count"), g("newnews", "truncated"))),
            ("NEWNEWS seconds", g("newnews", "seconds")),
            ("HEAD codes", [a.get("code") for a in (g("head", "articles") or [])]),
            ("STAT codes", [a.get("code") for a in (g("stat", "results") or [])]),
            ("XZVER code", g("xzver", "code")),
            ("XFEATURE COMPRESS GZIP code", g("xfeature_compress_gzip", "code")),
            ("errors", len(self.res["errors"])),
        ]
        return [[k, v] for k, v in rows]


def _dig(d, *ks):
    for k in ks:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def scrub(text, secrets):
    for s in secrets:
        if s and len(s) >= 3:
            text = text.replace(s, "[redacted]")
    return text


def main(argv=None):
    ap = argparse.ArgumentParser(description="Read-only NNTP capability probe")
    ap.add_argument("--host", required=True)
    ap.add_argument("--port", type=int, default=None, help="default 563 (TLS) or 119 with --no-tls")
    ap.add_argument("--user", default=None, help="username; password is read from env NNTP_PASS")
    ap.add_argument("--group", default="alt.binaries.test")
    ap.add_argument("--out", default="results.json")
    ap.add_argument("--no-tls", dest="tls", action="store_false")
    ap.add_argument("--insecure-tls", action="store_true", help="skip certificate verification")
    ap.add_argument("--timeout", type=float, default=20.0)
    ap.add_argument("--max-lines", type=int, default=40, help="raw lines kept per listing response")
    ap.add_argument("--over-count", type=int, default=200)
    ap.add_argument("--hdr-count", type=int, default=20)
    ap.add_argument("--newnews-minutes", type=int, default=10)
    ap.add_argument("--newnews-cap", type=int, default=2000)
    ap.add_argument("--no-compress-tests", action="store_true", help="skip XZVER and XFEATURE")
    ap.add_argument("--headers", default="NNTP-Posting-Date,X-Received-Body-CRC,Injection-Info,Path")
    a = ap.parse_args(argv)
    a.headers = [h.strip() for h in a.headers.split(",") if h.strip()]
    if a.port is None:
        a.port = 563 if a.tls else 119
    pw = os.environ.get("NNTP_PASS", "")
    if a.user and not pw:
        print("warning: --user given but NNTP_PASS is empty", file=sys.stderr)

    res = Probe(a, pw).run()
    text = scrub(json.dumps(res, indent=2, default=str), [pw])
    fd = os.open(a.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as fh:
        fh.write(text + "\n")

    print("\nNNTP probe %s  %s:%s tls=%s group=%s" % (VERSION, a.host, a.port, a.tls, a.group))
    if res.get("fatal"):
        print("FATAL: " + scrub(res["fatal"], [pw]))
    rows = res.get("summary") or []
    w = max([len(r[0]) for r in rows] + [10])
    print("-" * (w + 60))
    for k, v in rows:
        v = scrub(v if isinstance(v, str) else json.dumps(v, default=str), [pw])
        print("%-*s | %s" % (w, k, v[:140]))
    print("-" * (w + 60))
    for e in res.get("errors", []):
        print("error in %s: %s" % (e["step"], scrub(e["error"], [pw])))
    print("full results written to %s" % a.out)
    return 2 if res.get("fatal") else 0


if __name__ == "__main__":
    sys.exit(main())
