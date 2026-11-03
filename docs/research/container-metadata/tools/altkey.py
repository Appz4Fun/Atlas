#!/usr/bin/env python3
"""altkey.py: container-independent ALT keys for Matroska/WebM and ISOBMFF (MP4/MOV), stdlib only.

Usage:
  python3 -I altkey.py [--limit BYTES] FILE [FILE ...]          print keys per file
  python3 -I altkey.py [--limit BYTES] --ref REF FILE [...]     print match matrix vs REF

--limit BYTES bounds how far into the file payloads are read (Atlas-style prefix).
Keys (16 hex chars each, sha256 prefix):
  NOTE: byte-budget keys (first N bytes of video) were dropped: they depend on how much of the
  file the interleave places in the prefix, so they differ across muxers (see w6b.md).
  cfg_avcc     sha of the H.264 avcC record (MKV CodecPrivate == MP4 avcC payload)
  cfg_parm     sha of SPS+PPS NAL bytes (framing/avcC version independent)
  ac3_norm     sha of normalized AC-3 syncframe header fields (fscod,bsid,bsmod,acmod,lfeon)
  v_f0         sha of first video sample payload (AVCC length-prefixed, decode order)
  v_gop1       sha of video payloads from sample 0 up to the next sync sample
  v_n24        sha of first 24 video payloads (decode order); "shortK" if prefix holds fewer
  a_f100       sha of first 100 audio payloads; "shortK" if prefix holds fewer
  meta         normalized codec list|video frames|audio frames (full parse; "prefix" with --limit)
Also prints video NAL types (length-prefixed) and SEI user_data_unregistered info for frame 0.
Limitations: MKV unknown-size Cluster not followed; MP4 fragmented (moof) not parsed;
lacing is parsed (Xiph/EBML/fixed) but per-frame keys assume one payload per frame.
"""
import hashlib
import struct
import sys

LIMIT = None  # bytes; None = whole file
NORM = {"V_MPEG4/ISO/AVC": "H264", "avc1": "H264", "A_AC3": "AC3", "ac-3": "AC3",
        "S_TEXT/UTF8": "TEXT", "tx3g": "TEXT", "mp4a": "AAC", "A_AAC": "AAC"}


def h(b):
    return hashlib.sha256(b).hexdigest()[:16]


# ---------------- EBML / Matroska ----------------
def ebml_id(f):
    b = f.read(1)
    if not b:
        return None, 0
    first, n, mask = b[0], 1, 0x80
    while n <= 4 and not (first & mask):
        mask >>= 1
        n += 1
    if n > 4:
        return None, 0
    return int.from_bytes(b + f.read(n - 1), "big"), n


def ebml_size(f):
    b = f.read(1)
    if not b:
        return None, 0, False
    first, n, mask = b[0], 1, 0x80
    while n <= 8 and not (first & mask):
        mask >>= 1
        n += 1
    if n > 8:
        return None, 0, False
    v = first & (mask - 1)
    rest = f.read(n - 1)
    for c in rest:
        v = (v << 8) | c
    return v, n, v == (1 << (7 * n)) - 1


def ebml_children(f, start, end):
    """Yield (id, data_start, data_end) for elements in [start, end)."""
    pos = start
    while pos < end:
        f.seek(pos)
        eid, il = ebml_id(f)
        if not eid:
            return
        size, sl, unknown = ebml_size(f)
        if size is None:
            return
        ds = pos + il + sl
        de = end if unknown else min(ds + size, end)
        yield eid, ds, de
        if unknown:
            return
        pos = de


def ebml_uint(f, ds, de):
    f.seek(ds)
    return int.from_bytes(f.read(de - ds), "big") if de > ds else 0


def ebml_str(f, ds, de):
    f.seek(ds)
    return f.read(de - ds).rstrip(b"\0").decode("utf-8", "replace")


def ebml_bin(f, ds, de):
    f.seek(ds)
    return f.read(de - ds)


def vint_at(buf, i, signed=False):
    first, n, mask = buf[i], 1, 0x80
    while n <= 8 and not (first & mask):
        mask >>= 1
        n += 1
    v = first & (mask - 1)
    for k in range(1, n):
        v = (v << 8) | buf[i + k]
    if signed:
        v -= (1 << (7 * n - 1)) - 1
    return v, n


def split_lacing(buf, i, lacing):
    if lacing == 0:
        return [buf[i:]]
    n = buf[i] + 1
    i += 1
    if lacing == 2:  # fixed
        sz = (len(buf) - i) // n
        return [buf[i + k * sz:i + (k + 1) * sz] for k in range(n)]
    sizes = []
    if lacing == 1:  # Xiph
        for _ in range(n - 1):
            s = 0
            while True:
                c = buf[i]
                i += 1
                s += c
                if c != 255:
                    break
            sizes.append(s)
    else:  # EBML
        s, k = vint_at(buf, i)
        i += k
        sizes.append(s)
        for _ in range(n - 2):
            d, k = vint_at(buf, i, signed=True)
            i += k
            s += d
            sizes.append(s)
    sizes.append(len(buf) - i - sum(sizes))
    out, p = [], i
    for s in sizes:
        out.append(buf[p:p + s])
        p += s
    return out


def parse_mkv(f, fsize, limit):
    tracks, tcs = {}, 1000000
    blocks = []  # (track, flag_key, frames[list of bytes])
    timeline = []
    for eid, ds, de in ebml_children(f, 0, fsize):
        if eid != 0x18538067:  # Segment
            continue
        for sid, sds, sde in ebml_children(f, ds, de):
            if sid == 0x2AD7B1:
                tcs = ebml_uint(f, sds, sde)
            elif sid == 0x1654AE6B:  # Tracks
                for tid, tds, tde in ebml_children(f, sds, sde):
                    if tid != 0xAE:
                        continue
                    t = {"num": 0, "type": 0, "codec": "", "priv": b"", "dur": 0}
                    for xid, xds, xde in ebml_children(f, tds, tde):
                        if xid == 0xD7:
                            t["num"] = ebml_uint(f, xds, xde)
                        elif xid == 0x83:
                            t["type"] = ebml_uint(f, xds, xde)
                        elif xid == 0x86:
                            t["codec"] = ebml_str(f, xds, xde)
                        elif xid == 0x63A2:
                            t["priv"] = ebml_bin(f, xds, xde)
                        elif xid == 0x23E383:
                            t["dur"] = ebml_uint(f, xds, xde)
                    tracks[t["num"]] = t
            elif sid == 0x1F43B675:  # Cluster
                if limit is not None and sds > limit:
                    break
                ctc = 0
                for cid, cds, cde in ebml_children(f, sds, sde):
                    if cid == 0xE7:
                        ctc = ebml_uint(f, cds, cde)
                    elif cid in (0xA3, 0xA1):  # SimpleBlock / Block
                        if limit is not None and cde > limit:
                            return tracks, blocks
                        f.seek(cds)
                        buf = f.read(cde - cds)
                        tn, tl = vint_at(buf, 0)
                        rel = struct.unpack(">h", buf[tl:tl + 2])[0]
                        flags = buf[tl + 2]
                        key = bool(flags & 0x80) if cid == 0xA3 else True
                        frames = split_lacing(buf, tl + 3, (flags >> 1) & 3)
                        blocks.append((tn, key, frames))
                        timeline.append(((ctc + rel) * tcs / 1e6))
                    elif cid == 0xA0:  # BlockGroup
                        for bid, bds, bde in ebml_children(f, cds, cde):
                            if bid == 0xA1:
                                f.seek(bds)
                                buf = f.read(bde - bds)
                                tn, tl = vint_at(buf, 0)
                                flags = buf[tl + 2]
                                frames = split_lacing(buf, tl + 3, (flags >> 1) & 3)
                                blocks.append((tn, True, frames))
    return tracks, blocks


# ---------------- ISOBMFF / MP4 ----------------
def mp4_boxes(f, start, end):
    pos = start
    while pos + 8 <= end:
        f.seek(pos)
        size, typ = struct.unpack(">I4s", f.read(8))
        hl = 8
        if size == 1:
            size = struct.unpack(">Q", f.read(8))[0]
            hl = 16
        elif size == 0:
            size = end - pos
        if size < hl:
            return
        yield typ.decode("latin1"), pos + hl, min(pos + size, end), pos
        pos += size


def _u32s(f, off, n):
    f.seek(off)
    return struct.unpack(">%dI" % n, f.read(4 * n))


def parse_mp4(f, fsize, limit):
    tracks = []
    moov = next((b for b in mp4_boxes(f, 0, fsize) if b[0] == "moov"), None)
    if not moov or (limit is not None and moov[2] > limit):
        return [], None
    for typ, ds, de, _ in mp4_boxes(f, moov[1], moov[2]):
        if typ != "trak":
            continue
        t = {"id": 0, "handler": "", "codec": "", "entry": None, "sizes": [], "offs": [],
             "stss": None, "dur": []}
        stsc = []
        for bt, bds, bde, _ in mp4_boxes(f, ds, de):
            if bt == "tkhd":
                f.seek(bds)
                v = f.read(1)[0]
                f.seek(bds + 4 + (16 if v == 1 else 8))
                t["id"] = struct.unpack(">I", f.read(4))[0]
            elif bt == "mdia":
                for mt, mds, mde, _ in mp4_boxes(f, bds, bde):
                    if mt == "hdlr":
                        f.seek(mds + 8)
                        t["handler"] = f.read(4).decode("latin1")
                    elif mt == "minf":
                        for st, sds, sde, _ in mp4_boxes(f, mds, mde):
                            if st != "stbl":
                                continue
                            for xt, xds, xde, _ in mp4_boxes(f, sds, sde):
                                if xt == "stsd":
                                    f.seek(xds + 4)
                                    n = struct.unpack(">I", f.read(4))[0]
                                    eb = next(mp4_boxes(f, xds + 8, sde))
                                    t["codec"] = eb[0]
                                    t["entry"] = (eb[1], eb[2], eb[0])
                                elif xt == "stsz":
                                    f.seek(xds + 4)
                                    fixed, cnt = struct.unpack(">II", f.read(8))
                                    if fixed:
                                        t["sizes"] = [fixed] * cnt
                                    else:
                                        t["sizes"] = list(_u32s(f, xds + 12, cnt))
                                elif xt == "stsc":
                                    f.seek(xds + 4)
                                    n = struct.unpack(">I", f.read(4))[0]
                                    stsc = [struct.unpack(">III", f.read(12)) for _ in range(n)]
                                elif xt in ("stco", "co64"):
                                    f.seek(xds + 4)
                                    n = struct.unpack(">I", f.read(4))[0]
                                    if xt == "stco":
                                        t["offs"] = list(_u32s(f, xds + 8, n))
                                    else:
                                        f.seek(xds + 8)
                                        t["offs"] = list(struct.unpack(">%dQ" % n, f.read(8 * n)))
                                elif xt == "stss":
                                    f.seek(xds + 4)
                                    n = struct.unpack(">I", f.read(4))[0]
                                    t["stss"] = set(_u32s(f, xds + 8, n))
                                elif xt == "stts":
                                    f.seek(xds + 4)
                                    n = struct.unpack(">I", f.read(4))[0]
                                    for _ in range(n):
                                        c, d = struct.unpack(">II", f.read(8))
                                        t["dur"].append((c, d))
        # expand chunk table into per-sample absolute offsets
        offs, sidx = [], 0
        for ci, chunk_off in enumerate(t["offs"]):
            spc = 0
            for first, s_per, _ in stsc:
                if ci + 1 >= first:
                    spc = s_per
            pos = chunk_off
            for _ in range(spc):
                if sidx >= len(t["sizes"]):
                    break
                offs.append(pos)
                pos += t["sizes"][sidx]
                sidx += 1
        t["offsets"] = offs
        tracks.append(t)
    return tracks, None


def mp4_sample(f, t, i):
    f.seek(t["offsets"][i])
    return f.read(t["sizes"][i])


# ---------------- helpers ----------------
def avcc_parts(priv):
    """Return (lenSize, [sps,pps...]) from an avcC record."""
    if len(priv) < 7:
        return 4, []
    lsz = (priv[4] & 3) + 1
    p, out = 5, []
    nsps = priv[p] & 0x1F
    p += 1
    for _ in range(nsps):
        l = struct.unpack(">H", priv[p:p + 2])[0]
        out.append(priv[p + 2:p + 2 + l])
        p += 2 + l
    if p < len(priv):
        npps = priv[p]
        p += 1
        for _ in range(npps):
            l = struct.unpack(">H", priv[p:p + 2])[0]
            out.append(priv[p + 2:p + 2 + l])
            p += 2 + l
    return lsz, out


def nal_scan(sample, lsz):
    out, p = [], 0
    while p + lsz <= len(sample):
        l = int.from_bytes(sample[p:p + lsz], "big")
        nal = sample[p + lsz:p + lsz + l]
        out.append(nal)
        p += lsz + l
    return out


def sei_udu(nals):
    """List of SEI user_data_unregistered payload sizes (type 5) found in SEI NALs (type 6)."""
    res = []
    for n in nals:
        if not n or (n[0] & 0x1F) != 6:
            continue
        p = 1
        while p < len(n) - 1:
            pt = 0
            while n[p] == 0xFF:
                pt += 255
                p += 1
            pt += n[p]
            p += 1
            ps = 0
            while n[p] == 0xFF:
                ps += 255
                p += 1
            ps += n[p]
            p += 1
            if pt == 5:
                res.append((ps, n[p:p + 16].hex()[:32]))
            p += ps
    return res


def ac3_norm(frame):
    if len(frame) < 8 or frame[:2] != b"\x0b\x77":
        return b""
    bits = int.from_bytes(frame[4:8], "big")  # crc1 is 2 bytes at [2:4]
    v = int.from_bytes(frame[:8], "big")
    # bit offset 16 (after syncword) + 16 (crc1) = 32 -> fscod(2) frmsizecode(6) bsid(5) bsmod(3) acmod(3)
    b = int.from_bytes(frame[:8], "big")
    fscod = (b >> (64 - 32 - 2)) & 3
    bsid = (b >> (64 - 32 - 8 - 5)) & 0x1F
    bsmod = (b >> (64 - 32 - 8 - 5 - 3)) & 7
    acmod = (b >> (64 - 32 - 8 - 5 - 3 - 3)) & 7
    # lfeon position depends on cmixlev/surmixlev/dsurmod presence
    pos = 64 - 32 - 8 - 5 - 3 - 3  # bits remaining before the next field
    nb = pos
    if (acmod & 1) and acmod != 1:
        nb -= 2
    if acmod & 4:
        nb -= 2
    if acmod == 2:
        nb -= 2
    lfe = (b >> (nb - 1)) & 1 if nb >= 1 else 0
    return bytes([fscod, bsid, bsmod, acmod, lfe])


def analyse(path, limit):
    with open(path, "rb") as f:
        f.seek(0, 2)
        fsize = f.tell()
        f.seek(0)
        magic = f.read(4)
        out = {"file": path, "container": "?"}
        vs, as_ = [], []  # (payload, is_sync) video; payloads audio, decode order, prefix-visible only
        cfg_avcc = b""
        if magic == b"\x1a\x45\xdf\xa3":
            out["container"] = "mkv"
            tracks, blocks = parse_mkv(f, fsize, limit)
            vt = next((t for t in tracks.values() if t["codec"].startswith("V_MPEG4/ISO/AVC")), None)
            at = next((t for t in tracks.values() if t["codec"].startswith("A_AC3")), None)
            out["codecs"] = sorted(t["codec"] for t in tracks.values())
            out["codecs_norm"] = sorted(NORM.get(t["codec"], t["codec"]) for t in tracks.values())
            if vt:
                cfg_avcc = vt["priv"]
            for tn, key, frames in blocks:
                for fr in frames:
                    if vt and tn == vt["num"]:
                        vs.append((fr, key))
                    elif at and tn == at["num"]:
                        as_.append(fr)
        else:
            out["container"] = "mp4"
            tracks, _ = parse_mp4(f, fsize, limit)
            vt = next((t for t in tracks if t["codec"] == "avc1"), None)
            at = next((t for t in tracks if t["codec"] == "ac-3"), None)
            out["codecs"] = sorted(t["codec"] for t in tracks)
            out["codecs_norm"] = sorted(NORM.get(t["codec"], t["codec"]) for t in tracks)
            if vt:
                for bt, bds, bde, _ in mp4_boxes(f, vt["entry"][0] + 78, vt["entry"][1]):
                    if bt == "avcC":
                        f.seek(bds)
                        cfg_avcc = f.read(bde - bds)
                for i in range(len(vt["offsets"])):
                    if limit is not None and vt["offsets"][i] + vt["sizes"][i] > limit:
                        break
                    vs.append((mp4_sample(f, vt, i), (vt["stss"] is None) or ((i + 1) in vt["stss"])))
            if at:
                for i in range(len(at["offsets"])):
                    if limit is not None and at["offsets"][i] + at["sizes"][i] > limit:
                        break
                    as_.append(mp4_sample(f, at, i))
        n_v, n_a = len(vs), len(as_)
        lsz, parms = avcc_parts(cfg_avcc)
        cfg_parm = b"".join(parms)
        f0 = vs[0][0] if vs else b""
        gop = []
        for fr, key in vs:
            if gop and key:
                break
            gop.append(fr)
        v_gop1 = b"".join(gop)
        # frame-count keys (container-independent; prefix-coverage reported as nV/nA)
        v24 = [x[0] for x in vs[:24]]
        a100 = as_[:100]
        keys = {
            "cfg_avcc": h(cfg_avcc),
            "cfg_parm": h(cfg_parm),
            "ac3_norm": h(ac3_norm(as_[0]) if as_ else b""),
            "v_f0": h(f0),
            "v_gop1": h(v_gop1) if gop and (len(gop) < 24 or limit is None or True) else "n/a",
            "v_n24": h(b"".join(v24)) if len(v24) == 24 else "short%d" % len(v24),
            "a_f100": h(b"".join(a100)) if len(a100) == 100 else "short%d" % len(a100),
            "meta": h(("|".join(out["codecs_norm"]) + "|%d|%d" % (n_v, n_a)).encode()) if limit is None else "prefix",
        }
        out.update(keys)
        out["n_video"], out["n_audio"] = n_v, n_a
        out["v_frame0_bytes"] = len(f0)
        out["gop1_bytes"] = len(v_gop1)
        out["first_gop_frames"] = len(gop)
        out["v_nal_types_f0"] = [n[0] & 0x1F for n in nal_scan(f0, lsz) if n] if f0 else []
        out["sei_udu_f0"] = sei_udu(nal_scan(f0, lsz)) if f0 else []
        out["avcc_len"] = len(cfg_avcc)
        return out


def main(argv):
    global LIMIT
    ref = None
    files = []
    i = 0
    while i < len(argv):
        if argv[i] == "--limit":
            LIMIT = int(argv[i + 1])
            i += 2
        elif argv[i] == "--ref":
            ref = argv[i + 1]
            i += 2
        else:
            files.append(argv[i])
            i += 1
    res = {p: analyse(p, LIMIT) for p in ([ref] if ref else []) + files}
    keys = ["cfg_avcc", "cfg_parm", "ac3_norm", "v_f0", "v_gop1", "v_n24", "a_f100", "meta"]
    if ref:
        r = res[ref]
        print("%-16s %s" % ("variant", " ".join("%-9s" % k for k in keys)))
        for p in files:
            row = []
            for k in keys:
                row.append("%-9s" % ("match" if res[p][k] == r[k] else "NO"))
            print("%-16s %s  nV=%d nA=%d" % (p.split("/")[-1], " ".join(row), res[p]["n_video"], res[p]["n_audio"]))
    for p in ([ref] if ref else []) + files:
        r = res[p]
        print(p, r["container"], r["codecs_norm"], "frames v/a=%d/%d" % (r["n_video"], r["n_audio"]))
        print("  " + " ".join("%s=%s" % (k, r[k]) for k in keys))
        print("  f0_bytes=%d gop1_bytes=%d gop_frames=%d nal_types_f0=%s sei_udu_f0=%s avcC_len=%d" % (
            r["v_frame0_bytes"], r["gop1_bytes"], r["first_gop_frames"], r["v_nal_types_f0"],
            r["sei_udu_f0"], r["avcc_len"]))


if __name__ == "__main__":
    main(sys.argv[1:])
