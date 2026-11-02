#!/usr/bin/env python3
"""walk.py: stdlib-only Matroska/WebM (EBML) and ISOBMFF (MP4/M4V/MOV) structure walker.

Headers only: payloads are skipped by seeking, except small scalar/string fields,
CodecPrivate/BlockAddIDExtraData (length + short hex), and MP4 sample-entry fields.

Usage:  python3 -I walk.py FILE [FILE ...] [--prefix 700000 --prefix 4000000]
Prints top-level order with offsets/sizes, Info, Tracks, Chapters, Attachments,
Tags, Cues, SeekHead targets, Void, and, for each cut-off in --prefix, which
structures fit entirely inside the prefix.
"""
import os
import struct
import sys

# ---------------- EBML / Matroska ----------------
# id -> (name, kind). kind: m=master, u=uint, i=int, f=float, s=string, b=binary, d=date
MKV = {
    0x1A45DFA3: ("EBML", "m"), 0x4286: ("EBMLVersion", "u"), 0x42F7: ("EBMLReadVersion", "u"),
    0x42F2: ("EBMLMaxIDLength", "u"), 0x42F3: ("EBMLMaxSizeLength", "u"),
    0x4282: ("DocType", "s"), 0x4287: ("DocTypeVersion", "u"), 0x4285: ("DocTypeReadVersion", "u"),
    0x18538067: ("Segment", "m"), 0x114D9B74: ("SeekHead", "m"), 0x4DBB: ("Seek", "m"),
    0x53AB: ("SeekID", "b"), 0x53AC: ("SeekPosition", "u"), 0xEC: ("Void", "b"), 0xBF: ("CRC-32", "b"),
    0x1549A966: ("Info", "m"), 0x2AD7B1: ("TimecodeScale", "u"), 0x4489: ("Duration", "f"),
    0x4D80: ("MuxingApp", "s"), 0x5741: ("WritingApp", "s"), 0x4461: ("DateUTC", "d"),
    0x73A4: ("SegmentUID", "b"), 0x7BA9: ("Title", "s"),
    0x1654AE6B: ("Tracks", "m"), 0xAE: ("TrackEntry", "m"), 0xD7: ("TrackNumber", "u"),
    0x73C5: ("TrackUID", "u"), 0x83: ("TrackType", "u"), 0x86: ("CodecID", "s"),
    0x63A2: ("CodecPrivate", "b"), 0x258688: ("CodecName", "s"), 0x22B59C: ("Language", "s"),
    0x22B59D: ("LanguageBCP47", "s"), 0x536E: ("Name", "s"), 0xB9: ("FlagEnabled", "u"),
    0x88: ("FlagDefault", "u"), 0x55AA: ("FlagForced", "u"), 0x9C: ("FlagLacing", "u"),
    0x23E383: ("DefaultDuration", "u"), 0x6DE7: ("MinCache", "u"), 0x56AA: ("CodecDelay", "u"),
    0x56BB: ("SeekPreRoll", "u"), 0x6D80: ("ContentEncodings", "m"), 0x6240: ("ContentEncoding", "m"),
    0x5034: ("ContentCompression", "m"), 0x5035: ("ContentEncryption", "m"),
    0x41E4: ("BlockAdditionMapping", "m"), 0x41F0: ("BlockAddIDValue", "u"),
    0x41E7: ("BlockAddIDType", "u"), 0x41E8: ("BlockAddIDName", "s"), 0x41ED: ("BlockAddIDExtraData", "b"),
    0xE0: ("Video", "m"), 0xB0: ("PixelWidth", "u"), 0xBA: ("PixelHeight", "u"),
    0x54B0: ("DisplayWidth", "u"), 0x54BA: ("DisplayHeight", "u"), 0x9A: ("FlagInterlaced", "u"),
    0x53B8: ("StereoMode", "u"), 0x55B0: ("Colour", "m"), 0x55B1: ("MatrixCoefficients", "u"),
    0x55B2: ("BitsPerChannel", "u"), 0x55B9: ("Range", "u"), 0x55BA: ("TransferCharacteristics", "u"),
    0x55BB: ("Primaries", "u"), 0x55BC: ("MaxCLL", "u"), 0x55BD: ("MaxFALL", "u"),
    0x55D0: ("MasteringMetadata", "m"), 0x55D1: ("PrimaryRChromaticityX", "f"),
    0x55D2: ("PrimaryRChromaticityY", "f"), 0x55D3: ("PrimaryGChromaticityX", "f"),
    0x55D4: ("PrimaryGChromaticityY", "f"), 0x55D5: ("PrimaryBChromaticityX", "f"),
    0x55D6: ("PrimaryBChromaticityY", "f"), 0x55D7: ("WhitePointChromaticityX", "f"),
    0x55D8: ("WhitePointChromaticityY", "f"), 0x55D9: ("LuminanceMax", "f"), 0x55DA: ("LuminanceMin", "f"),
    0xE1: ("Audio", "m"), 0xB5: ("SamplingFrequency", "f"), 0x78B5: ("OutputSamplingFrequency", "f"),
    0x9F: ("Channels", "u"), 0x7D7B: ("ChannelPositions", "b"), 0x6264: ("BitDepth", "u"),
    0x1043A770: ("Chapters", "m"), 0x45B9: ("EditionEntry", "m"), 0x45BC: ("EditionUID", "u"),
    0x45DB: ("EditionFlagDefault", "u"), 0xB6: ("ChapterAtom", "m"), 0x73C4: ("ChapterUID", "u"),
    0x91: ("ChapterTimeStart", "u"), 0x92: ("ChapterTimeEnd", "u"), 0x80: ("ChapterDisplay", "m"),
    0x85: ("ChapString", "s"), 0x437C: ("ChapLanguage", "s"), 0x437E: ("ChapCountry", "s"),
    0x1941A469: ("Attachments", "m"), 0x61A7: ("AttachedFile", "m"), 0x467E: ("FileDescription", "s"),
    0x466E: ("FileName", "s"), 0x4660: ("FileMediaType", "s"), 0x465C: ("FileData", "b"),
    0x46AE: ("FileUID", "u"),
    0x1254C367: ("Tags", "m"), 0x7373: ("Tag", "m"), 0x63C0: ("Targets", "m"),
    0x68CA: ("TargetTypeValue", "u"), 0x63CA: ("TargetType", "s"), 0x63C5: ("TagTrackUID", "u"),
    0x67C8: ("SimpleTag", "m"), 0x45A3: ("TagName", "s"), 0x447A: ("TagLanguage", "s"),
    0x4484: ("TagDefault", "u"), 0x4487: ("TagString", "s"), 0x4485: ("TagBinary", "b"),
    0x1C53BB6B: ("Cues", "m"), 0xBB: ("CuePoint", "m"), 0xB3: ("CueTime", "u"),
    0x1F43B675: ("Cluster", "m"), 0xE7: ("Timecode", "u"), 0xA3: ("SimpleBlock", "b"),
    0xA0: ("BlockGroup", "m"), 0xA1: ("Block", "b"), 0xFB: ("BlockAdditions", "m"),
}
SEG_CHILDREN = {0x114D9B74: "SeekHead", 0x1549A966: "Info", 0x1654AE6B: "Tracks",
                0x1043A770: "Chapters", 0x1941A469: "Attachments", 0x1254C367: "Tags",
                0x1C53BB6B: "Cues", 0x1F43B675: "Cluster", 0xEC: "Void", 0xBF: "CRC-32"}
PREFIXES_DEFAULT = [700_000, 4_000_000]


def ebml_header(f, pos, limit):
    """Return (id, id_len, size, size_len, unknown) or None."""
    f.seek(pos)
    b = f.read(1)
    if not b or b[0] == 0:
        return None
    idlen = 9 - b[0].bit_length()
    if idlen > 4:
        return None
    rest = f.read(idlen - 1)
    if len(rest) != idlen - 1:
        return None
    eid = int.from_bytes(b + rest, "big")
    sb = f.read(1)
    if not sb or sb[0] == 0:
        return None
    slen = 9 - sb[0].bit_length()
    srest = f.read(slen - 1)
    if len(srest) != slen - 1:
        return None
    raw = sb[0] & ((1 << (8 - slen)) - 1)
    val = (raw << (8 * (slen - 1))) | int.from_bytes(srest, "big")
    unknown = val == (1 << (7 * slen)) - 1
    return eid, idlen, val, idlen + slen, unknown


def ebml_children(f, start, end):
    """Yield (id, elem_offset, data_offset, data_size, truncated) for each child."""
    pos = start
    while pos < end:
        h = ebml_header(f, pos, end)
        if h is None:
            return
        eid, _il, size, hlen, unknown = h
        data = pos + hlen
        if unknown:
            size = end - data
            trunc = False
        else:
            trunc = data + size > end
            size = min(size, max(0, end - data))
        yield eid, pos, data, size, trunc
        if size == 0 and hlen == 0:
            return
        pos = data + size


def read_value(f, off, size, kind):
    f.seek(off)
    if kind == "u" or kind == "d":
        v = int.from_bytes(f.read(min(size, 8)), "big")
        return v if kind == "u" else v  # date: ns since 2001-01-01 (signed) -> kept raw
    if kind == "i":
        return int.from_bytes(f.read(min(size, 8)), "big", signed=True)
    if kind == "f":
        raw = f.read(size)
        if size == 4:
            return struct.unpack(">f", raw)[0]
        if size == 8:
            return struct.unpack(">d", raw)[0]
        return None
    if kind == "s":
        return f.read(min(size, 512)).split(b"\x00")[0].decode("utf-8", "replace")
    if kind == "b":
        return f.read(min(size, 48))
    return None


def hx(b, n=24):
    return b.hex() + ("..." if len(b) >= n else "")


def walk_mkv(f, fsize, out, prefixes):
    def emit(s=""):
        out.append(s)

    # EBML header
    hdr = list(ebml_children(f, 0, fsize))
    if not hdr or hdr[0][0] != 0x1A45DFA3:
        emit("Not EBML")
        return
    eid, off, data, size, _ = hdr[0]
    doc = {}
    for cid, co, cd, cs, _t in ebml_children(f, data, data + size):
        name, kind = MKV.get(cid, ("0x%X" % cid, "b"))
        if kind in "us" or kind == "f":
            doc[name] = read_value(f, cd, cs, kind)
    emit("[EBML header] @%d size=%d DocType=%s DocTypeVersion=%s" % (
        off, size, doc.get("DocType"), doc.get("DocTypeVersion")))
    segs = [c for c in hdr if c[0] == 0x18538067]
    if not segs:
        emit("No Segment found")
        return
    _, seg_off, seg_data, seg_size, seg_trunc = segs[0]
    seg_end = seg_data + seg_size if seg_size else fsize
    emit("[Segment] @%d data@%d size=%d%s" % (seg_off, seg_data, seg_size,
                                              " (unknown/live)" if seg_size == 0 else ""))
    top = []
    clusters = []
    info = {}
    tracks = []
    chapters = []
    attachments = []
    tags_list = []
    cues_count = 0
    seek_targets = []
    voids = []
    for cid, co, cd, cs, tr in ebml_children(f, seg_data, seg_end):
        nm = SEG_CHILDREN.get(cid, "0x%X" % cid)
        top.append((nm, co, cd, cs, tr))
        if cid == 0x1F43B675:
            clusters.append((co, cd + cs))
        elif cid == 0x114D9B74:  # SeekHead
            for sid, so, sd, ss, _ in ebml_children(f, cd, cd + cs):
                if sid != 0x4DBB:
                    continue
                tgt, pos = None, None
                for xid, xo, xd, xs, _ in ebml_children(f, sd, sd + ss):
                    if xid == 0x53AB:
                        tgt = int.from_bytes(f.read(0) or b"", "big")
                        f.seek(xd); tgt = int.from_bytes(f.read(xs), "big")
                    elif xid == 0x53AC:
                        f.seek(xd); pos = int.from_bytes(f.read(xs), "big")
                if tgt is not None and pos is not None:
                    seek_targets.append((tgt, pos, seg_data + pos))
        elif cid == 0x1549A966:
            for xid, xo, xd, xs, _ in ebml_children(f, cd, cd + cs):
                n, k = MKV.get(xid, ("0x%X" % xid, "b"))
                if k in "usfd":
                    info[n] = read_value(f, xd, xs, k)
        elif cid == 0x1654AE6B:
            for xid, xo, xd, xs, _ in ebml_children(f, cd, cd + cs):
                if xid == 0xAE:
                    t = {"@": xo, "size": xs, "codec_private": 0, "bam": [], "lang": None}
                    for yid, yo, yd, ys, _ in ebml_children(f, xd, xd + xs):
                        n, k = MKV.get(yid, ("0x%X" % yid, "b"))
                        if yid == 0x63A2:
                            t["codec_private"] = ys
                            f.seek(yd); t["cp_head"] = f.read(min(ys, 8)).hex()
                        elif yid == 0x41E4:
                            bam = {}
                            for zid, zo, zd, zs, _ in ebml_children(f, yd, yd + ys):
                                zn, zk = MKV.get(zid, ("0x%X" % zid, "b"))
                                if zid == 0x41ED:
                                    bam["extra_len"] = zs
                                    f.seek(zd); bam["extra_head"] = f.read(min(zs, 16)).hex()
                                elif zk in "us":
                                    bam[zn] = read_value(f, zd, zs, zk)
                            t["bam"].append(bam)
                        elif yid in (0xE0, 0xE1):
                            sub = {}
                            for zid, zo, zd, zs, _ in ebml_children(f, yd, yd + ys):
                                zn, zk = MKV.get(zid, ("0x%X" % zid, "b"))
                                if zid == 0x55B0:
                                    col = {}
                                    for wid, wo, wd, ws, _ in ebml_children(f, zd, zd + zs):
                                        wn, wk = MKV.get(wid, ("0x%X" % wid, "b"))
                                        if wid == 0x55D0:
                                            md = {}
                                            for vid, vo, vd, vs, _ in ebml_children(f, wd, wd + ws):
                                                vn, vk = MKV.get(vid, ("0x%X" % vid, "b"))
                                                md[vn] = read_value(f, vd, vs, vk)
                                            col["MasteringMetadata"] = md
                                        elif wk in "uf":
                                            col[wn] = read_value(f, wd, ws, wk)
                                    sub["Colour"] = col
                                elif zid in (0x54B0, 0x54BA, 0xB0, 0xBA, 0xB5, 0x9F, 0x6264, 0x9A):
                                    sub[zn] = read_value(f, zd, zs, zk)
                                elif zk in "uf":
                                    sub[zn] = read_value(f, zd, zs, zk)
                            t["av"] = sub
                        elif yid in (0x86, 0x536E, 0x22B59C, 0x22B59D, 0xD7, 0x73C5, 0x83, 0x88, 0x55AA, 0xB9, 0x23E383):
                            t[n] = read_value(f, yd, ys, k)
                    tracks.append(t)
        elif cid == 0x1043A770:
            for eid_, eo, ed, es, _ in ebml_children(f, cd, cd + cs):
                if eid_ != 0x45B9:
                    continue
                for aid, ao, ad, as_, _ in ebml_children(f, ed, ed + es):
                    if aid != 0xB6:
                        continue
                    ch = {"@": ao, "size": as_, "names": [], "start": None}
                    for bid, bo, bd, bs, _ in ebml_children(f, ad, ad + as_):
                        if bid == 0x91:
                            ch["start"] = read_value(f, bd, bs, "u")
                        elif bid == 0x80:
                            for cid2, co2, cd2, cs2, _ in ebml_children(f, bd, bd + bs):
                                if cid2 == 0x85:
                                    ch["names"].append(read_value(f, cd2, cs2, "s"))
                                elif cid2 == 0x437C:
                                    ch.setdefault("lang", read_value(f, cd2, cs2, "s"))
                    chapters.append(ch)
        elif cid == 0x1941A469:
            for aid, ao, ad, as_, _ in ebml_children(f, cd, cd + cs):
                if aid != 0x61A7:
                    continue
                a = {"@": ao, "size": as_}
                for bid, bo, bd, bs, _ in ebml_children(f, ad, ad + as_):
                    bn, bk = MKV.get(bid, ("0x%X" % bid, "b"))
                    if bid == 0x465C:
                        a["data_off"] = bd; a["data_size"] = bs
                    elif bk in "us":
                        a[bn] = read_value(f, bd, bs, bk)
                attachments.append(a)
        elif cid == 0x1254C367:
            for tid, to, td, ts, _ in ebml_children(f, cd, cd + cs):
                if tid != 0x7373:
                    continue
                tg = {"@": to, "size": ts, "simple": [], "target": None}
                for sid, so, sd, ss, _ in ebml_children(f, td, td + ts):
                    if sid == 0x63C0:
                        tv = None
                        for xid, xo, xd, xs, _ in ebml_children(f, sd, sd + ss):
                            if xid == 0x68CA:
                                tv = read_value(f, xd, xs, "u")
                        tg["target"] = tv
                    elif sid == 0x67C8:
                        nm = val = lang = None
                        for xid, xo, xd, xs, _ in ebml_children(f, sd, sd + ss):
                            if xid == 0x45A3:
                                nm = read_value(f, xd, xs, "s")
                            elif xid == 0x4487:
                                val = read_value(f, xd, xs, "s")
                            elif xid == 0x447A:
                                lang = read_value(f, xd, xs, "s")
                        tg["simple"].append((nm, val, lang))
                tags_list.append(tg)
        elif cid == 0x1C53BB6B:
            cues_count = sum(1 for x in ebml_children(f, cd, cd + cs) if x[0] == 0xBB)
        elif cid == 0xEC:
            voids.append((co, cs))

    # ---- report ----
    emit("\n[Top-level Segment children, in file order]")
    run = None
    for nm, co, cd, cs, tr in top:
        if nm == "Cluster":
            if run is None:
                run = [nm, co, co, 1, cs]
            else:
                run[2] = co; run[3] += 1
            continue
        if run is not None:
            emit("  Cluster x%d  first@%d  last@%d  (sizes of first: %d)" % (run[3], run[1], run[2], run[4]))
            run = None
        emit("  %-12s @%-10d data@%-10d size=%d%s" % (nm, co, cd, cs, " TRUNCATED" if tr else ""))
    if run is not None:
        emit("  Cluster x%d  first@%d  last@%d" % (run[3], run[1], run[2]))

    emit("\n[SeekHead targets]")
    for tgt, pos, absolute in seek_targets:
        nm = {0x1549A966: "Info", 0x1654AE6B: "Tracks", 0x1043A770: "Chapters", 0x1941A469: "Attachments",
              0x1254C367: "Tags", 0x1C53BB6B: "Cues", 0x1F43B675: "Cluster", 0x114D9B74: "SeekHead"}.get(tgt, hex(tgt))
        emit("  %-12s SeekPosition=%d -> abs %d  %s" % (nm, pos, absolute,
                                                       "(tail, >=%d)" % (absolute) if absolute > 4_000_000 else ""))
    if voids:
        emit("  Void: " + ", ".join("@%d size=%d" % v for v in voids))

    emit("\n[Info]")
    if info.get("TimecodeScale") is None:
        info["TimecodeScale"] = 1000000  # Matroska default when element absent
    for k in ("TimecodeScale", "Duration", "MuxingApp", "WritingApp", "DateUTC", "Title", "SegmentUID"):
        v = info.get(k)
        if k == "SegmentUID" and v is not None:
            v = v.hex()
        if k == "Duration" and v is not None and info.get("TimecodeScale"):
            v = "%s  (~%.3f s at TimecodeScale %s)" % (v, v * info["TimecodeScale"] / 1e9, info["TimecodeScale"])
        if k == "TimecodeScale" and info.get("TimecodeScale") == 1000000 and "2AD7B1" not in repr(info.keys()):
            v = "1000000 (default, element absent)"
        emit("  %-13s %s" % (k, v))
    if info.get("DateUTC") is not None:
        emit("  DateUTC raw ns since 2001-01-01: %d" % info["DateUTC"])

    emit("\n[Tracks] (%d)" % len(tracks))
    for t in tracks:
        av = t.get("av", {})
        extra = ""
        if av.get("Colour"):
            c = av["Colour"]
            md = c.get("MasteringMetadata", {})
            extra = " colour=" + repr({k: v for k, v in c.items() if k != "MasteringMetadata"})
            if md:
                extra += " mastering=" + repr({k: (round(v, 4) if isinstance(v, float) else v) for k, v in md.items() if v is not None})
        emit("  #%s type=%s codec=%s lang=%s/%s name=%r default=%s forced=%s CodecPrivate=%dB head=%s" % (
            t.get("TrackNumber"), t.get("TrackType"), t.get("CodecID"), t.get("Language"),
            t.get("LanguageBCP47"), t.get("Name"), t.get("FlagDefault"), t.get("FlagForced"),
            t.get("codec_private", 0), t.get("cp_head")))
        if av:
            emit("      av=" + repr({k: v for k, v in av.items() if k != "Colour"}) + extra)
        for b in t.get("bam", []):
            emit("      BlockAdditionMapping=" + repr(b))
        emit("      track @%d size=%d" % (t["@"], t["size"]))

    emit("\n[Chapters] %s" % ("none" if not chapters else "%d atoms" % len(chapters)))
    for ch in chapters[:20]:
        emit("  @%d size=%d start=%s names=%r" % (ch["@"], ch["size"], ch["start"], ch["names"]))

    emit("\n[Attachments] (%d)" % len(attachments))
    for a in attachments:
        emit("  @%d size=%d name=%r mime=%r data@%s data_size=%s" % (
            a["@"], a["size"], a.get("FileName"), a.get("FileMediaType"), a.get("data_off"), a.get("data_size")))

    emit("\n[Tags] (%d Tag elements)" % len(tags_list))
    for tg in tags_list[:40]:
        emit("  Tag @%d size=%d target=%s n=%d" % (tg["@"], tg["size"], tg["target"], len(tg["simple"])))
        for nm, val, lang in tg["simple"][:8]:
            emit("      %s=%r (%s)" % (nm, (val or "")[:80] if isinstance(val, str) else val, lang))
    emit("\n[Cues] %d CuePoints" % cues_count)

    # ---- prefix analysis ----
    emit("\n[Prefix fit] (element fits entirely in prefix if end <= limit)")
    items = []
    if clusters:
        items.append(("first Cluster END (header+first cluster)", clusters[0][1]))
        items.append(("last Cluster end", clusters[-1][1]))
    for nm, co, cd, cs, tr in top:
        if nm != "Cluster":
            items.append((nm + " end", co + (cd - co) + cs))
    items.append(("(all Tracks/Info/SeekHead/Chapters/Attachments/Tags/Cues end)", max(
        [co + (cd - co) + cs for nm, co, cd, cs, tr in top if nm != "Cluster"] or [0])))
    for lim in prefixes:
        emit("  limit %d bytes:" % lim)
        for label, end in items:
            emit("    %-62s end=%-10d %s" % (label, end, "IN" if end <= lim else "OUT"))
    if clusters:
        emit("  first Cluster starts @%d (%.1f KB from file start)" % (clusters[0][0], clusters[0][0] / 1024))


# ---------------- ISOBMFF / MP4 ----------------
CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"edts", b"dinf", b"moof", b"traf",
              b"mvex", b"mfra", b"sinf", b"schi", b"ilst", b"tref", b"iprp", b"ipco", b"meta", b"stsd",
              b"wave", b"sgpd", b"sbgp", b"dref", b"clip"}
FULLBOX_CONTAINERS = {b"meta": 4, b"stsd": 8, b"dref": 8, b"stsd": 8}
VISUAL_SAMPLE = {b"avc1", b"avc3", b"hev1", b"hvc1", b"dvh1", b"dvhe", b"encv", b"av01", b"vp09", b"mp4v"}
AUDIO_SAMPLE = {b"mp4a", b"ec-3", b"ac-3", b"enca", b"alac", b"Opus", b"fLaC", b"dtsc", b"dtsh", b"dtse", b"ec+3"}
SKIP_DETAIL = {b"stts", b"stsc", b"stss", b"ctts", b"sdtp", b"stsz", b"stz2", b"stco", b"co64", b"sgpd", b"sbgp"}


def mp4_boxes(f, start, end, depth, out, ctx, fsize, moov_info):
    pos = start
    while pos + 8 <= end:
        f.seek(pos)
        hdr = f.read(16)
        if len(hdr) < 8:
            break
        size, typ = struct.unpack(">I4s", hdr[:8])
        hlen = 8
        if size == 1:
            if len(hdr) < 16:
                break
            size = struct.unpack(">Q", hdr[8:16])[0]; hlen = 16
        elif size == 0:
            size = end - pos
        if size < hlen or pos + size > end + 0:
            out.append("%s%s @%d size=%d (INVALID or exceeds parent end %d)" % ("  " * depth, typ.decode("latin1"), pos, size, end))
            break
        ind = "  " * depth
        if typ == b"uuid":
            f.seek(pos + hlen); out.append("%suuid @%d size=%d usertype=%s" % (ind, pos, size, f.read(16).hex()))
        else:
            out.append("%s%s @%d size=%d" % (ind, typ.decode("latin1"), pos, size))
        body = pos + hlen
        bend = pos + size
        t = typ
        if t == b"mdat":
            ctx.setdefault("mdat", []).append((pos, size))
        elif t == b"moov":
            ctx["moov"] = (pos, size)
        elif t == b"mvhd":
            f.seek(body); v = f.read(1)[0]
            if v == 1:
                f.seek(body + 4 + 16); ts, dur = struct.unpack(">IQ", f.read(12))
            else:
                f.seek(body + 4 + 8); ts, dur = struct.unpack(">II", f.read(8))
            out.append("%s  mvhd timescale=%d duration=%d (%.3f s)" % (ind, ts, dur, dur / ts if ts else 0))
        elif t == b"tkhd":
            f.seek(body); v = f.read(1)[0]
            f.seek(body + 4 + (16 if v == 1 else 8)); tid = struct.unpack(">I", f.read(4))[0]
            f.seek(bend - 8); w, h = struct.unpack(">II", f.read(8))
            out.append("%s  tkhd track_ID=%d display=%.2fx%.2f" % (ind, tid, w / 65536, h / 65536))
            ctx["tid"] = tid
        elif t == b"mdhd":
            f.seek(body); v = f.read(1)[0]
            if v == 1:
                f.seek(body + 4 + 16); ts, dur = struct.unpack(">IQ", f.read(12))
            else:
                f.seek(body + 4 + 8); ts, dur = struct.unpack(">II", f.read(8))
            f.seek(body + (32 if v == 1 else 20)); lc = struct.unpack(">H", f.read(2))[0]
            lang = "".join(chr(((lc >> s) & 0x1f) + 0x60) for s in (10, 5, 0))
            out.append("%s  mdhd timescale=%d duration=%d lang=%s" % (ind, ts, dur, lang))
        elif t == b"hdlr":
            f.seek(body + 8); ht = f.read(4).decode("latin1")
            f.seek(body + 24); name = f.read(bend - body - 24).split(b"\x00")[0].decode("utf-8", "replace")
            ctx["handler"] = ht
            out.append("%s  hdlr handler=%s name=%r" % (ind, ht, name))
        elif t == b"stsz":
            f.seek(body + 4); ssz, cnt = struct.unpack(">II", f.read(8))
            out.append("%s  stsz sample_count=%d uniform_size=%d" % (ind, cnt, ssz))
        elif t in (b"stco", b"co64"):
            f.seek(body + 4); cnt = struct.unpack(">I", f.read(4))[0]
            fmt = ">I" if t == b"stco" else ">Q"
            n = struct.calcsize(fmt)
            first = struct.unpack(fmt, f.read(n))[0] if cnt else None
            out.append("%s  %s chunk_count=%d first_chunk_offset=%s" % (ind, t.decode(), cnt, first))
            ctx.setdefault("first_chunk", []).append(first)
        elif t == b"stts" or t == b"stss" or t == b"ctts" or t == b"stsc":
            f.seek(body + 4); cnt = struct.unpack(">I", f.read(4))[0]
            out.append("%s  %s entries=%d" % (ind, t.decode(), cnt))
        elif t == b"btrt":
            f.seek(body); b, mx, av = struct.unpack(">III", f.read(12))
            out.append("%s  btrt bufferSizeDB=%d maxBitrate=%d avgBitrate=%d" % (ind, b, mx, av))
        elif t == b"colr":
            f.seek(body); ct = f.read(4)
            if ct == b"nclx":
                p, tr, m, fr = struct.unpack(">HHHB", f.read(7)[:7])
                out.append("%s  colr nclx primaries=%d transfer=%d matrix=%d full_range=%d" % (ind, p, tr, m, fr >> 7))
            else:
                out.append("%s  colr %s raw=%s" % (ind, ct.decode("latin1"), f.read(bend - body - 4).hex()[:64]))
        elif t == b"mdcv":
            f.seek(body)
            raw = f.read(24)
            if len(raw) == 24:
                vals = struct.unpack(">HHHHHHHHII", raw)
                out.append("%s  mdcv primaries(xy,raw)=%r wp=%r maxLum=%.4fcd/m2 minLum=%.6fcd/m2" % (
                    ind, vals[0:6], vals[6:8], vals[8] / 10000, vals[9] / 10000))
        elif t == b"clli":
            f.seek(body); mc, mf = struct.unpack(">HH", f.read(4))
            out.append("%s  clli maxCLL=%d maxFALL=%d" % (ind, mc, mf))
        elif t in (b"dvcC", b"dvvC", b"dvwC"):
            f.seek(body); raw = f.read(min(bend - body, 24))
            maj, mnr = raw[0], raw[1]
            b3 = int.from_bytes(raw[2:5], "big")
            prof = (b3 >> 17) & 0x7F; lvl = (b3 >> 11) & 0x3F
            rpu, el, bl = (b3 >> 10) & 1, (b3 >> 9) & 1, (b3 >> 8) & 1
            comp = (b3 >> 4) & 0xF
            out.append("%s  %s v%d.%d profile=%d level=%d rpu=%d el=%d bl=%d compat_id=%d raw=%s" % (
                ind, t.decode(), maj, mnr, prof, lvl, rpu, el, bl, comp, raw.hex()))
        elif t == b"avcC" or t == b"hvcC":
            f.seek(body); out.append("%s  %s len=%d head=%s" % (ind, t.decode(), bend - body, f.read(min(12, bend - body)).hex()))
        elif t == b"dec3":
            f.seek(body); out.append("%s  dec3 EC3SpecificBox raw=%s" % (ind, f.read(min(bend - body, 16)).hex()))
        elif t == b"dac3":
            f.seek(body); out.append("%s  dac3 raw=%s" % (ind, f.read(min(bend - body, 8)).hex()))
        elif t == b"esds":
            f.seek(body + 4); buf = f.read(min(bend - body - 4, 64))
            out.append("%s  esds len=%d head=%s" % (ind, bend - body, buf.hex()[:80]))
        elif t == b"chnl":
            f.seek(body); out.append("%s  chnl raw=%s" % (ind, f.read(min(bend - body, 16)).hex()))
        elif t == b"udta" or t == b"meta" or t == b"ilst":
            pass
        if t.startswith(b"\xa9") or (ctx.get("in_ilst") and t not in (b"data",)):
            pass
        if t in VISUAL_SAMPLE or (ctx.get("handler") == "vide" and t == b"stsd"):
            pass

        if t == b"stsd":
            f.seek(body); cnt = struct.unpack(">I", f.read(4) + b"")[0] if False else None
            f.seek(body + 4); ecount = struct.unpack(">I", f.read(4))[0]
            entries_start = body + 8
            ent = entries_start
            for _ in range(ecount):
                f.seek(ent); eh = f.read(8)
                if len(eh) < 8:
                    break
                es, et = struct.unpack(">I4s", eh)
                if es < 8:
                    break
                ebody = ent + 8
                eend = ent + es
                desc = ""
                if et in VISUAL_SAMPLE:
                    f.seek(ebody + 24); w, h = struct.unpack(">HH", f.read(4))
                    desc = "visual %dx%d" % (w, h)
                    child_off = ebody + 78
                elif et in AUDIO_SAMPLE:
                    f.seek(ebody + 8); ver = struct.unpack(">H", f.read(2))[0]
                    f.seek(ebody + 16); ch, bits = struct.unpack(">HH", f.read(4))
                    f.seek(ebody + 24); sr = struct.unpack(">I", f.read(4))[0] >> 16
                    desc = "audio channels=%d bits=%d samplerate=%d ver=%d" % (ch, bits, sr, ver)
                    child_off = ebody + (28 if ver == 0 else 44 if ver == 1 else 64)
                else:
                    child_off = ebody + 8
                out.append("%s  sample_entry %s %s @%d size=%d" % (ind, et.decode("latin1"), desc, ent, es))
                if child_off < eend:
                    mp4_boxes(f, child_off, eend, depth + 2, out, ctx, fsize, moov_info)
                ent = eend
            ctx.pop("handler", None) if False else None
        elif t in CONTAINERS and t not in (b"meta", b"stsd"):
            if t in (b"sgpd", b"sbgp", b"dref"):
                pass
            elif t == b"ilst":
                ctx["in_ilst"] = True
                mp4_boxes(f, body, bend, depth + 1, out, ctx, fsize, moov_info)
                ctx["in_ilst"] = False
            elif t == b"udta" or t == b"ilst" or t == b"meta":
                mp4_boxes(f, body, bend, depth + 1, out, ctx, fsize, moov_info)
            else:
                mp4_boxes(f, body, bend, depth + 1, out, ctx, fsize, moov_info)
        elif t == b"meta":
            mp4_boxes(f, body + 4, bend, depth + 1, out, ctx, fsize, moov_info)
        elif ctx.get("in_ilst") and typ[0:1] in (b"\xa9",) or (ctx.get("in_ilst") and t not in (b"data",)):
            # iTunes-style metadata item: child 'data' box holds the value
            f.seek(body)
            subhdr = f.read(8)
            if len(subhdr) == 8 and subhdr[4:8] == b"data":
                ds = struct.unpack(">I", subhdr[:4])[0]
                f.seek(body + 8); dtype = struct.unpack(">I", f.read(4))[0]
                f.seek(body + 16); payload = f.read(min(ds - 16, 200))
                val = payload.decode("utf-8", "replace") if dtype == 1 else payload[:24].hex()
                out.append("%s  item %s type=%d value=%r" % (ind, t.decode("latin1"), dtype, val))
        pos = pos + size
        if size == 0:
            break


def walk_mp4(f, fsize, out, prefixes):
    ctx = {}
    out_lines = []
    mp4_boxes(f, 0, fsize, 0, out_lines, ctx, fsize, None)
    out.extend(out_lines)
    out.append("\n[MP4 summary]")
    moov = ctx.get("moov")
    mdats = ctx.get("mdat", [])
    if moov:
        out.append("  moov @%d size=%d end=%d" % (moov[0], moov[1], moov[0] + moov[1]))
    for m in mdats[:4]:
        out.append("  mdat @%d size=%d" % m)
    fc = ctx.get("first_chunk") or []
    if fc:
        out.append("  first sample chunk offsets (stco/co64): %s" % fc[:4])
    if moov and mdats:
        faststart = moov[0] < mdats[0][0]
        out.append("  faststart (moov before first mdat): %s" % faststart)
    for lim in prefixes:
        if moov:
            out.append("  limit %d: moov %s" % (lim, "IN" if moov[0] + moov[1] <= lim else "OUT (end=%d)" % (moov[0] + moov[1])))


def main(argv):
    prefixes = []
    files = []
    i = 0
    while i < len(argv):
        if argv[i] == "--prefix":
            prefixes.append(int(argv[i + 1])); i += 2
        else:
            files.append(argv[i]); i += 1
    if not prefixes:
        prefixes = PREFIXES_DEFAULT
    for path in files:
        out = ["=" * 78, "FILE %s  size=%d" % (path, os.path.getsize(path)), "=" * 78]
        try:
            with open(path, "rb") as f:
                fsize = os.path.getsize(path)
                f.seek(0)
                magic = f.read(12)
                if magic[:4] == b"\x1a\x45\xdf\xa3":
                    walk_mkv(f, fsize, out, prefixes)
                elif magic[4:8] in (b"ftyp", b"moov", b"mdat", b"free", b"wide", b"skip"):
                    walk_mp4(f, fsize, out, prefixes)
                else:
                    out.append("Unknown container magic %r" % magic[:12])
        except Exception as e:  # keep going on malformed files
            out.append("ERROR while walking: %r" % (e,))
        print("\n".join(out))
        print()


if __name__ == "__main__":
    main(sys.argv[1:])
