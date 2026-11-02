#!/usr/bin/env python3
"""Per-NZB posting-behaviour metrics. Usage: python3 -I nzb_stats.py <file.nzb>"""
import collections
import re
import sys
import xml.etree.ElementTree as ET
from statistics import median

NS = {"n": "http://www.newzbin.com/DTD/2003/nzb"}


def parse(path):
    with open(path, "rb") as f:
        raw = f.read()
    # strip DOCTYPE so no external DTD is ever considered
    raw = re.sub(rb"<!DOCTYPE[^>]*>", b"", raw, count=1)
    root = ET.fromstring(raw)
    files = []
    for fe in root.iter("{http://www.newzbin.com/DTD/2003/nzb}file"):
        subj = fe.get("subject", "")
        segs = []
        for se in fe.iter("{http://www.newzbin.com/DTD/2003/nzb}segment"):
            segs.append((int(se.get("number")), int(se.get("bytes", 0)), (se.text or "").strip()))
        segs.sort()
        groups = [g.text for g in fe.iter("{http://www.newzbin.com/DTD/2003/nzb}group")]
        files.append({
            "poster": fe.get("poster", ""),
            "date": int(fe.get("date", 0)),
            "subject": subj,
            "groups": groups,
            "segs": segs,
        })
    return files


def ext_of(subject):
    m = re.search(r'"([^"]+)"', subject)
    name = m.group(1) if m else subject
    m2 = list(re.finditer(r"\.([A-Za-z0-9]{1,5})\b", name))
    m2 = m2[-1] if m2 else None
    if not m2:
        return "none"
    e = m2.group(1).lower()
    if re.fullmatch(r"r\d\d|part\d+|\d{3}", e) or e in ("rar",):
        return "rar"
    if e in ("7z",) or re.fullmatch(r"\d{3}", e):
        return "7z"
    return e


def mid_class(mid):
    if "@" not in mid:
        return "no-@", ""
    local, dom = mid.rsplit("@", 1)
    if re.fullmatch(r"[0-9a-f]{32}", local):
        shape = "hex32"
    elif re.fullmatch(r"[A-Za-z0-9]{8,}\.\d+\$[A-Za-z0-9]+\.\d+@\S+", mid.split("@")[0] + "@") or "$" in local:
        shape = "xref-style(id$id.n)"
    elif re.fullmatch(r"[0-9A-Za-z]{10,}", local):
        shape = "alnum-token"
    else:
        shape = "other"
    return shape, dom


def main(path):
    files = parse(path)
    posters = collections.Counter(f["poster"] for f in files)
    groups = collections.Counter(g for f in files for g in f["groups"])
    group_sets = collections.Counter(tuple(sorted(f["groups"])) for f in files)
    mids = [s[2] for f in files for s in f["segs"]]
    shapes = collections.Counter(mid_class(m)[0] for m in mids)
    doms = collections.Counter(mid_class(m)[1] for m in mids)

    # bytes distribution
    sizes_all = [s[1] for f in files for s in f["segs"]]
    size_mode = collections.Counter(sizes_all).most_common(1)[0] if sizes_all else ("-", 0)
    per_file_last = []
    per_file_nseg = []
    per_file_maxsz = []
    for f in files:
        if not f["segs"]:
            continue
        per_file_nseg.append(len(f["segs"]))
        per_file_last.append(f["segs"][-1][1])
        per_file_maxsz.append(max(s[1] for s in f["segs"]))
    fsz = collections.Counter(per_file_maxsz).most_common(1)
    all_same_full = len(set(per_file_maxsz)) == 1 if per_file_maxsz else False
    # last-segment sizes: how many differ from the file's full size
    short_last = sum(1 for f, l, m in zip(files, per_file_last, per_file_maxsz) if l < m)

    # dates
    dated = sorted((f["date"], i) for i, f in enumerate(files))
    file_order = [i for _, i in dated]
    # monotone = file order in NZB equals date order
    monotone = all(files[file_order[k]] is files[k] or True for k in range(len(files)))
    nzb_order_dates = [f["date"] for f in files]
    is_monotone_nzb = all(nzb_order_dates[k] <= nzb_order_dates[k + 1] for k in range(len(nzb_order_dates) - 1))
    gaps = [nzb_order_dates[k + 1] - nzb_order_dates[k] for k in range(len(nzb_order_dates) - 1)]
    span = (max(nzb_order_dates) - min(nzb_order_dates)) if nzb_order_dates else 0
    exts = collections.Counter(ext_of(f["subject"]) for f in files)

    print(f"FILE {path}")
    print(f"  files={len(files)} segments={len(mids)}")
    print(f"  posters={len(posters)} top={posters.most_common(3)}")
    print(f"  groups={dict(groups)} distinct_group_sets={len(group_sets)}")
    print(f"  mid_shapes={dict(shapes)} domains={dict(doms.most_common(4))}")
    print(f"  seg_bytes mode={size_mode[0]} (x{size_mode[1]}) min={min(sizes_all) if sizes_all else 0} max={max(sizes_all) if sizes_all else 0}")
    print(f"  per-file max seg size mode={fsz[0] if fsz else '-'} all_files_same_full_size={all_same_full}")
    print(f"  per-file segs median={median(per_file_nseg) if per_file_nseg else 0} last_seg_shorter_than_full={short_last}/{len(per_file_last)}")
    print(f"  date monotone_in_nzb_order={is_monotone_nzb} span_s={span} gap_s min={min(gaps) if gaps else 0} median={median(gaps) if gaps else 0} max={max(gaps) if gaps else 0}")
    print(f"  ext_mix={dict(exts)}")
    print(f"  subject_sample={files[0]['subject'][:110] if files else ''}")


if __name__ == "__main__":
    main(sys.argv[1])
