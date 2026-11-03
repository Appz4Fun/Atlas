import re, collections, os
W = "/tmp/claude-0/-home-claude/a4d53905-78b9-58a1-82a6-938c4e3c790c/scratchpad/w7a"
# ordered: first match wins
RULES = [
 ("nyuu",            r"^(?:[A-Z][a-z]){12}-\d{13}@nyuu$"),
 ("nyuu_loose",      r"^[A-Za-z]{24}-\d{13}@nyuu$"),
 ("private",         r"-\d{13}@PRiVATE$"),
 ("nyuu_style_otherhost", r"^[A-Za-z]{24}-\d{13}@[^@]+$"),   # nyuu-shaped prefix, non-nyuu host
 ("ngpost",          r"^[0-9a-f]{32}@ngPost$"),
 ("gopoststuff",     r"^\d+\$gps@gopoststuff$"),
 ("newsmangler",     r"^\d{10}\.\d{5}\.\d+@"),
 ("postie",          r"^[A-Za-z0-9]{32}@[A-Za-z0-9]{8}\.[A-Za-z0-9]{3}$"),
 ("nxg",             r"^[0-9a-f]{40}@[0-9a-f]{21}\.[0-9a-f]{3}$"),
 ("jbinup",          r"@JBinUp\.local$"),
 ("powerpost",       r"@powerpost2000AA\.local$"),
 ("highwinds",       r"^[0-9A-Za-z_%]{4,6}\.\d+\$[0-9A-Za-z_%]+\.\d+@fx\d+\.[a-z]+\d*$"),
 ("diablo",          r"^[0-9a-f]{8}\$\d+\$\d+\$[0-9a-f]+(\$\w+)?@"),
 ("inn_refined",     r"^[0-9a-f]+\$[0-9a-v]+\$[0-9a-v]+\$[0-9a-f]+@"),
 ("usenetfarm",      r"^[A-Za-z]{32}@(UFK|news\.usenet\.farm)$"),
 ("giganews",        r"^[A-Za-z0-9_-]{32}@giganews\.com$"),
 ("giganews_other",  r"@giganews\.com$"),
 ("yencbin_partN",   r"^Part\d+of\d+\.[0-9A-F]{32}@\d{10}\.local$"),
 ("hex32_ts_host",   r"^[0-9a-f]{32}-\d{10,13}@"),
 ("ts_host_random",  r"^[A-Za-z0-9]{10,20}@\d{10}\.\d+[A-Za-z0-9]+$"),
 ("nntpposter",      r"^[0-9a-f]{32}@"),
 ("usenetfarm_loose",r"^[A-Za-z]{32}@"),
 ("hex3_dollar_7",   r"^[0-9a-f]{6,8}\$[0-9a-f]{6,8}\$[0-9a-f]{6,8}@[0-9a-f]{10}\.[0-9a-f]{5}$"),
 ("alnum_rand_host", r"^[A-Za-z0-9]{20,28}@[A-Za-z0-9]{5,13}\.[A-Za-z0-9]{3}$"),
 ("synthetic_a1",   r"^[a-z]\d*-\d+@"),
]
SYNTH_HOSTS = re.compile(r"@(?:[^@]*\.)?(?:example|example\.com|example\.invalid|test|nntune\.test|news\.example(?:\.invalid|\.com)?|nothereatall|nowhere\.example)$|^[^@]*@(?:news\.example|example)$")
COMP = [(n, re.compile(p)) for n, p in RULES]
def classify(mid):
    if "@" not in mid:
        return "no_at_sign"
    for n, rx in COMP:
        if rx.search(mid):
            return n
    return "unknown"
def shape(s):
    out = []
    for m in re.finditer(r"[A-Za-z0-9]+|.", s):
        t = m.group(0)
        if len(t) == 1 and not t.isalnum():
            out.append(t); continue
        if t.isdigit():
            k = "9"
        elif t.islower():
            k = "h" if re.fullmatch(r"[0-9a-f]+", t) and len(t) >= 8 else "a"
        elif t.isupper():
            k = "A"
        else:
            k = "x"
        out.append(f"{k}{{{len(t)}}}")
    return "".join(out)
def load():
    return [l.rstrip("\n").split("\t") for l in open(os.path.join(W,"segs.tsv"), encoding="utf-8", errors="replace")]
if __name__ == "__main__":
    rows = load()
    cls = collections.Counter(); nzbs = collections.defaultdict(set); synth = 0
    shapes = collections.Counter(); ex = {}
    for r in rows:
        nzb, date, mid = r[0], r[1], r[2]
        c = classify(mid)
        if c in ("nntpposter","usenetfarm_loose","unknown","ts_host_random","hex32_ts_host","giganews_other","nyuu_style_otherhost") and SYNTH_HOSTS.search(mid):
            synth += 1
        cls[c] += 1; nzbs[c].add(nzb)
        if c == "unknown":
            sh = shape(mid); shapes[sh] += 1; ex.setdefault(sh, mid)
    print("TOTAL", len(rows), "NZBS", len({r[0] for r in rows}), "synthetic-host rows", synth)
    for c, n in cls.most_common():
        print(f"{c}\t{n}\tnzbs={len(nzbs[c])}")
    print("--- top unknown shapes (of", sum(shapes.values()), ", distinct", len(shapes), ")")
    for sh, n in shapes.most_common(15):
        print(n, sh, " EX:", ex[sh])
