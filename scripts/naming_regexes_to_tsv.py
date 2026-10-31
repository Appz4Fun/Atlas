#!/usr/bin/env python3
"""newznab-tmux ReleaseNamingRegexesTableSeeder.php -> data/naming_regexes.tsv and tests/fixtures/naming_samples.tsv.

Usage: scripts/naming_regexes_to_tsv.py SEEDER.php OUT.tsv SAMPLES.tsv
Source: newznab-tmux at f39acb5ed7fe90a77ab01b7f92e96b473ffe1628, GPLv3. Python 3.11 or newer.
"""
import re
import sys
import warnings

warnings.simplefilter("ignore", FutureWarning)  # re warns about the "[[" PCRE takes literally
STR = r"'((?:[^'\\]|\\.)*)'"
ROW = re.compile(
    r"'id'\s*=>\s*(\d+),\s*'group_regex'\s*=>\s*" + STR + r",\s*'regex'\s*=>\s*" + STR
    + r",\s*'status'\s*=>\s*(\d+),\s*'description'\s*=>\s*" + STR + r",\s*'ordinal'\s*=>\s*(-?\d+),",
    re.S,
)
QUANTIFIER = re.compile(r"\{\d+(?:,\d*)?\}")
NEEDS_BACKTRACKING = re.compile(r"\(\?(?:=|!|<=|<!|>|\(|R|\d|&|P=|P>)")


def unphp(s):
    """A PHP single-quoted literal: only \\\\ and \\' are escapes."""
    return re.sub(r"\\(['\\])", r"\1", s)


def split_delimiters(raw):
    raw = raw.strip()
    d, end = raw[0], raw.rfind(raw[0])
    flags = raw[end + 1 :].replace(" ", "")  # PHP ignores spaces among the flags
    if d not in "/#" or end == 0 or set(flags) - set("iu"):
        raise ValueError(f"unexpected delimiters or flags: {raw[:60]}")
    return raw[1:end], flags


def convert(b):
    """PCRE syntax to the regex crate's, keeping PCRE's meaning. Returns (pattern, what changed)."""
    out, changes, i, n, in_class, class_start, after_quantifier = [], [], 0, len(b), False, 0, False
    while i < n:
        c = b[i]
        if c == "\\":
            e = b[i + 1]
            if not in_class and e == "E":  # \E with no \Q means nothing to PCRE, the crate refuses it
                changes.append("dropped \\E")
                i += 2
                continue
            if e in "QGXRKgk" or (not in_class and e in "123456789"):
                raise ValueError(f"unsupported \\{e} in {b[:60]}")
            out.append(b[i : i + 2])
            i, after_quantifier = i + 2, False
        elif in_class:
            if c == "]" and i > class_start:
                in_class = False
                out.append(c)
            elif c == "]":  # first in a class: a literal to PCRE
                out.append("\\]")
                changes.append("escaped leading ] in a class")
            elif c == "[" and b[i + 1 : i + 2] != ":":  # a literal to PCRE, a nested class to the crate
                out.append("\\[")
                changes.append("escaped [ in a class")
            elif b[i : i + 2] in ("&&", "--", "~~"):
                raise ValueError(f"class set operator in {b[:60]}")
            else:
                out.append(c)
            i += 1
        elif c == "[":
            in_class, class_start = True, i + 1
            out.append(c)
            i += 1
            if b[i : i + 1] == "^":
                out.append("^")
                i, class_start = i + 1, i + 1
            after_quantifier = False
        elif c == "(" and NEEDS_BACKTRACKING.match(b, i):
            raise ValueError(f"needs backtracking: {b[:60]}")
        elif c in "+*?":
            if after_quantifier and c == "+":  # possessive: the crate would read x?+ as (x?)+
                changes.append("possessive quantifier made greedy")
                after_quantifier = False
            else:
                out.append(c)
                after_quantifier = not (after_quantifier and c == "?")
            i += 1
        elif c == "{":
            m = QUANTIFIER.match(b, i)
            if m:
                out.append(m.group(0))
                i, after_quantifier = m.end(), True
            else:  # a literal to PCRE, an error to the crate
                out.append("\\{")
                changes.append("escaped literal {")
                i, after_quantifier = i + 1, False
        else:
            out.append(c)
            i, after_quantifier = i + 1, False
    return "".join(out), changes


def sample_of(description, py):
    """(subject, expected) from the examples in a seeder description, by RegexService::_matchRegex's rule."""
    text = re.sub(r"^//\s*", "", description.split(":::")[-1].strip())
    for subject in re.split(r"\s+or\s+|\s+//\s*", text):
        m = py.search(subject.strip()) if "\t" not in subject else None
        if m:
            name = "".join(v or "" for k, v in sorted(m.groupdict().items()) if not re.search("reqid|parts", k, re.I))
            if name:
                return subject.strip(), name
    return None


def main(seeder, tsv, samples):
    rows, sample_rows, stats = [], [], {}
    for m in ROW.finditer(open(seeder, encoding="utf-8").read()):
        rid, group, raw, status, desc, ordinal = int(m[1]), unphp(m[2]), unphp(m[3]), int(m[4]), unphp(m[5]), int(m[6])
        if status != 1:
            stats["disabled"] = stats.get("disabled", 0) + 1
            continue
        try:
            re.compile(group)
        except re.error:  # rule 1131 has an unbalanced ")" in its group regex and can never apply
            stats["bad group regex"] = stats.get("bad group regex", 0) + 1
            continue
        body, flags = split_delimiters(raw)
        pattern, changes = convert(body)
        for c in changes:
            stats[c] = stats.get(c, 0) + 1
        if any(ch in pattern + group for ch in "\t\r\n"):
            raise ValueError(f"tab or newline in rule {rid}")
        rows.append((ordinal, group, rid, "i" if "i" in flags else "", pattern))
        try:
            py = re.compile(body.replace("\\E", ""), (re.I if "i" in flags else 0) | (0 if "u" in flags else re.A))
        except re.error:
            continue
        found = sample_of(desc, py)
        if found:
            sample_rows.append((rid, *found))
    rows.sort()
    with open(tsv, "w", encoding="utf-8", newline="\n") as f:
        f.write("# generated by scripts/naming_regexes_to_tsv.py, do not edit\n")
        f.write("# newznab-tmux database/seeders/ReleaseNamingRegexesTableSeeder.php at f39acb5, GPLv3\n")
        f.write("# id\tordinal\tgroup_regex\tflags\tpattern\n")
        f.writelines(f"{r}\t{o}\t{g}\t{fl}\t{p}\n" for o, g, r, fl, p in rows)
    with open(samples, "w", encoding="utf-8", newline="\n") as f:
        f.write("# generated by scripts/naming_regexes_to_tsv.py from the seeder's own descriptions\n")
        f.write("# id\tsubject\texpected name\n")
        f.writelines(f"{r}\t{s}\t{n}\n" for r, s, n in sorted(sample_rows))
    print(f"{len(rows)} rules, {len(sample_rows)} samples, {dict(sorted(stats.items()))}")


if __name__ == "__main__":
    main(*sys.argv[1:])
