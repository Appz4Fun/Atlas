#!/usr/bin/env python3
"""disc_ts_avi.py - stdlib-only probes (Atlas research, w6a).

Run with -I:
  python3 -I disc_ts_avi.py iso IMAGE.iso [--dump-dir DIR]
  python3 -I disc_ts_avi.py ts  FILE.ts|FILE.m2ts [--limit N]
  python3 -I disc_ts_avi.py avi FILE.avi [--limit N]

iso : ISO9660 PVD + VRS, UDF anchors/VDS/FSD, VIDEO_TS directory (ISO9660 and UDF),
      VIDEO_TS.IFO / VTS_xx_0.IFO attributes (BE fields), PGC playback time, DVDDiscID (MD5).
ts  : TS/M2TS packet size detection, PAT/PMT/SDT (service name/provider), ES descriptors.
avi : RIFF walk, avih/strh/strf/strn, INFO, JUNK, odml (dmlh, indx), first video chunk
      user-data strings (Lavc / XviD / DivX).
Only the first --limit bytes are read for ts/avi (default 700 KB / 4 MiB respectively).
"""
import hashlib
import os
import struct
import sys

SECT = 2048
KB = 1024


def u16le(b, o): return struct.unpack_from("<H", b, o)[0]
def u32le(b, o): return struct.unpack_from("<I", b, o)[0]
def u64le(b, o): return struct.unpack_from("<Q", b, o)[0]
def u16be(b, o): return struct.unpack_from(">H", b, o)[0]
def u32be(b, o): return struct.unpack_from(">I", b, o)[0]
def u64be(b, o): return struct.unpack_from(">Q", b, o)[0]


def bcd(x):
    return (x >> 4) * 10 + (x & 0x0F)


def crc16_ccitt(data):
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


def parse_tag(b):
    tid, ver, cks, _r, serial, crc, crclen, loc = struct.unpack_from("<HHBBHHHI", b, 0)
    cks_ok = ((sum(b[0:16]) - b[4]) & 0xFF) == cks
    crc_ok = crc16_ccitt(b[16:16 + crclen]) == crc
    return {"id": tid, "loc": loc, "tag_ok": cks_ok and crc_ok}


def dstring(b):
    """OSTA CS0 dstring: last byte is the field length including the compression byte."""
    if not b:
        return ""
    n = b[-1]
    if n == 0 or n > len(b):
        return ""
    data = b[:n]
    if data[0] == 8:
        return data[1:].decode("latin-1", "replace")
    if data[0] == 16:
        return data[1:].decode("utf-16-be", "replace")
    return repr(data)


def cstr(b):
    return b.split(b"\x00", 1)[0].decode("latin-1", "replace").strip()


def regid_id(b):
    return b[1:24].split(b"\x00", 1)[0].decode("latin-1", "replace")


class Img:
    def __init__(self, path):
        self.fh = open(path, "rb")
        self.fh.seek(0, 2)
        self.size = self.fh.tell()

    def read(self, off, n):
        self.fh.seek(off)
        return self.fh.read(n)

    def sector(self, lba, n=1):
        return self.read(lba * SECT, n * SECT)


# ---------------- ISO9660 ----------------

def iso_records(img, lba, size):
    data = img.read(lba * SECT, size)
    out, pos = [], 0
    while pos < len(data):
        ln = data[pos]
        if ln == 0:
            pos = (pos // SECT + 1) * SECT
            continue
        rec = data[pos:pos + ln]
        nlen = rec[32]
        out.append({
            "name": rec[33:33 + nlen].decode("latin-1", "replace") if nlen > 1 or rec[33] > 1 else repr(rec[33]),
            "lba": u32le(rec, 2), "size": u32le(rec, 10), "dir": bool(rec[25] & 2),
        })
        pos += ln
    return out


# ---------------- UDF ----------------

class UDF:
    def __init__(self, img, part_start):
        self.img = img
        self.part = part_start

    def read_fe(self, lbn):
        b = self.img.sector(self.part + lbn)
        t = parse_tag(b)
        if t["id"] not in (261, 266):
            return None
        info = u64le(b, 56)
        if t["id"] == 261:
            lea, lad, ad0 = u32le(b, 168), u32le(b, 172), 176
        else:
            lea, lad, ad0 = u32le(b, 208), u32le(b, 212), 216
        ad0 += lea
        adtype = u16le(b, 34) & 7
        ftype = b[27]
        ext, pos = [], ad0
        if adtype == 3:
            return {"tag": t["id"], "ftype": ftype, "info": info, "inline": True, "extents": [], "adtype": adtype, "tag_ok": t["tag_ok"]}
        while pos < ad0 + lad:
            if adtype == 0:
                ln, p = u32le(b, pos), u32le(b, pos + 4)
                pos += 8
            elif adtype == 1:
                ln, p = u32le(b, pos), u32le(b, pos + 4)
                pos += 16
            else:
                break
            ext.append((ln & 0x3FFFFFFF, self.part + p))
        return {"tag": t["id"], "ftype": ftype, "info": info, "extents": ext, "adtype": adtype, "tag_ok": t["tag_ok"]}

    def file_bytes(self, fe, whole_blocks=False):
        out = bytearray()
        for ln, lba in fe["extents"]:
            out += self.img.read(lba * SECT, ln)
        if whole_blocks:
            return bytes(out)
        return bytes(out[:fe["info"]])

    def dir_entries(self, fe):
        data = self.file_bytes(fe)
        out, pos = [], 0
        while pos + 38 <= len(data):
            chars = data[pos + 18]
            lfi = data[pos + 19]
            lbn = u32le(data, pos + 24)
            lu = u16le(data, pos + 36)
            nstart = pos + 38 + lu
            raw = data[nstart:nstart + lfi]
            if raw[:1] == b"\x10":
                name = raw[1:].decode("utf-16-be", "replace")
            elif raw[:1] == b"\x08":
                name = raw[1:].decode("latin-1", "replace")
            else:
                name = raw.decode("latin-1", "replace")
            if not chars & 0x08:  # skip parent entry
                out.append({"name": name, "lbn": lbn, "dir": bool(chars & 2), "hidden": bool(chars & 1)})
            pos += (38 + lu + lfi + 3) & ~3
        return out


# ---------------- DVD IFO ----------------

def video_attr(v):
    return {"coding": "MPEG-2" if (v[0] >> 6) & 3 == 1 else ("MPEG-1" if (v[0] >> 6) & 3 == 0 else "?"),
            "standard": "PAL" if (v[0] >> 4) & 3 == 1 else "NTSC",
            "aspect": "16:9" if (v[0] >> 2) & 3 == 3 else "4:3",
            "resolution_idx": (v[1] >> 3) & 7}


AUDIO_CODING = {0: "AC-3", 2: "MPEG-1", 3: "MPEG-2 ext", 4: "LPCM", 6: "DTS"}


def audio_attr(a):
    return {"coding": AUDIO_CODING.get((a[0] >> 5) & 7, "?%d" % ((a[0] >> 5) & 7)),
            "lang": (chr(a[2]) + chr(a[3])) if a[2] else "",
            "channels": (a[1] & 7) + 1,
            "sample_rate_code": (a[1] >> 4) & 3,
            "quant_code": (a[1] >> 6) & 3,
            "lang_ext": a[5]}


def pgc_playback(p):
    fps_flag = (p[7] >> 6) & 3
    fps = {1: "25", 3: "29.97"}.get(fps_flag, "?")
    return "%02d:%02d:%02d.%02d (fps flag %d -> %s)" % (
        bcd(p[4]), bcd(p[5]), bcd(p[6]), bcd(p[7] & 0x3F), fps_flag, fps)


def parse_ifo(b, kind):
    out = {"ident": b[0:12].decode("latin-1", "replace"), "bytes": len(b)}
    out["last_sector_vob"] = u32be(b, 0x0C)
    out["last_sector_ifo"] = u32be(b, 0x1C)
    if kind == "VMG":
        out["version_0x20_0x21"] = (b[0x20], b[0x21])
        out["category_0x22"] = "%08x" % u32be(b, 0x22)
        out["nr_volumes_0x26"] = u16be(b, 0x26)
        out["nr_title_sets_0x3E"] = u16be(b, 0x3E)
        out["provider_0x40"] = cstr(b[0x40:0x60])
        out["tt_srpt_sector_0xC4"] = u32be(b, 0xC4)
        out["vmgm_pgci_ut_sector_0xC8"] = u32be(b, 0xC8)
        return out
    # VTS
    out["vts_pgci_sector_0xCC"] = u32be(b, 0xCC)
    out["video_attr"] = video_attr(b[0x200:0x202])
    na = u16be(b, 0x202)
    out["audio_streams"] = [audio_attr(b[0x204 + 8 * i:0x20C + 8 * i]) for i in range(min(na, 8))]
    ns = u16be(b, 0x254)
    out["subpic_streams"] = []
    for i in range(min(ns, 32)):
        s = b[0x256 + 6 * i:0x25C + 6 * i]
        out["subpic_streams"].append({"coding": s[0] >> 5, "lang": (chr(s[2]) + chr(s[3])) if s[2] else "", "lang_ext": s[5]})
    pgci = u32be(b, 0xCC) * SECT
    if pgci:
        nlus = u16be(b, pgci)
        out["pgci_nr_lus"] = nlus
        if nlus:
            pgc_off = pgci + u32be(b, pgci + 12)
            p = b[pgc_off:pgc_off + 16]
            out["pgc0_nr_programs_chapters"] = p[2]
            out["pgc0_nr_cells"] = p[3]
            out["pgc0_playback_time"] = pgc_playback(p)
    return out


# ---------------- commands ----------------

def cmd_iso(path, dump_dir=None):
    img = Img(path)
    print("== ISO %s size=%d (%d sectors, %.2f MiB)" % (path, img.size, img.size // SECT, img.size / 2**20))
    vrs = []
    for s in range(16, 48):
        sec = img.sector(s)
        ident = sec[1:6]
        if ident in (b"BEA01", b"NSR02", b"NSR03", b"TEA01", b"CD001"):
            vrs.append("%d:%s(type %d)" % (s, ident.decode(), sec[0]))
    print("VRS sectors:", ", ".join(vrs))
    pvd = img.sector(16)
    total = u32le(pvd, 80)
    print("PVD@0x8000 type=%d id=%r blk=%d vol_space=%d" % (pvd[0], pvd[1:6], u16le(pvd, 128), total))
    print("  volid(40..72)=%r volsetid(190..318)=%r" % (cstr(pvd[40:72]), cstr(pvd[190:318])))
    print("  publisher=%r app=%r creation(813..830)=%r" % (cstr(pvd[318:446]), cstr(pvd[574:702]), pvd[813:829].decode("latin-1")))
    root = pvd[156:190]
    print("  root dir record extent=%d size=%d" % (u32le(root, 2), u32le(root, 10)))
    iso_root = iso_records(img, u32le(root, 2), u32le(root, 10))
    vts = [e for e in iso_root if e["name"] == "VIDEO_TS" and e["dir"]]
    if vts:
        print("ISO9660 VIDEO_TS: lba=%d byte_off=%d" % (vts[0]["lba"], vts[0]["lba"] * SECT))
        for e in iso_records(img, vts[0]["lba"], vts[0]["size"]):
            print("  iso %-16s lba=%-6d byte_off=%-10d size=%d %s" % (e["name"], e["lba"], e["lba"] * SECT, e["size"], "IN_FIRST_4MiB" if e["lba"] * SECT < 4 * 2**20 else "beyond 4MiB"))
    # UDF anchors
    anchors = []
    for cand in (256, total - 256, total - 1):
        sec = img.sector(cand)
        if u16le(sec, 0) == 2:
            anchors.append((cand, parse_tag(sec)["tag_ok"]))
    print("UDF AVDP anchors (sector, tag_ok):", anchors)
    avdp = img.sector(256)
    vds_len, vds_loc = u32le(avdp, 16), u32le(avdp, 20)
    print("AVDP main VDS: loc=%d len=%d bytes" % (vds_loc, vds_len))
    part_start, lvd, fsd_lbn, fsd_pref = None, None, None, None
    for i in range(vds_len // SECT):
        sec = img.sector(vds_loc + i)
        t = parse_tag(sec)
        if t["id"] == 1:
            print("  PVD(udf): volid=%r" % dstring(sec[24:56]))
        elif t["id"] == 4:
            print("  IUVD: impl=%r LVI=%r LVInfo1=%r LVInfo2=%r" % (regid_id(sec[20:52]), dstring(sec[116:244]), dstring(sec[244:280]), dstring(sec[280:316])))
        elif t["id"] == 5:
            pn = u16le(sec, 22)
            part_start = u32le(sec, 188)
            print("  PD: partnum=%d start_lba=%d len=%d (byte_off=%d) tag_ok=%s" % (pn, part_start, u32le(sec, 192), part_start * SECT, t["tag_ok"]))
        elif t["id"] == 6:
            lvd = sec
            fsd_lbn, fsd_pref = u32le(sec, 252), u16le(sec, 256)
            print("  LVD: lvid=%r block=%d FSD long_ad lbn=%d partref=%d nmaps=%d impl=%r" % (dstring(sec[84:212]), u32le(sec, 212), fsd_lbn, fsd_pref, u32le(sec, 268), regid_id(sec[272:304])))
        elif t["id"] == 8:
            break
    if part_start is None:
        print("no partition descriptor"); return
    fsd = img.sector(part_start + fsd_lbn)
    print("FSD tag id=%d tag_ok=%s root ICB lbn=%d" % (parse_tag(fsd)["id"], parse_tag(fsd)["tag_ok"], u32le(fsd, 404)))
    udf = UDF(img, part_start)
    root_fe = udf.read_fe(u32le(fsd, 404))
    rents = udf.dir_entries(root_fe)
    print("UDF root entries:", [(e["name"], e["dir"]) for e in rents])
    vts_e = [e for e in rents if e["name"] == "VIDEO_TS" and e["dir"]]
    if not vts_e:
        print("no UDF VIDEO_TS"); return
    vts_fe = udf.read_fe(vts_e[0]["lbn"])
    print("UDF VIDEO_TS FE tag=%d ICB lbn=%d byte_off=%d" % (vts_fe["tag"], vts_e[0]["lbn"], (part_start + vts_e[0]["lbn"]) * SECT))
    ifos = {}
    for e in udf.dir_entries(vts_fe):
        fe = udf.read_fe(e["lbn"])
        if fe is None:
            print("  udf %-14s (no FE)" % e["name"]); continue
        off = fe["extents"][0][1] * SECT if fe["extents"] else -1
        print("  udf %-14s FE=%d lbn=%d info=%d extents=%s first_byte_off=%d %s" % (
            e["name"], fe["tag"], e["lbn"], fe["info"], [(x[0], x[1]) for x in fe["extents"][:3]], off,
            "IN_FIRST_4MiB" if 0 <= off < 4 * 2**20 else "beyond 4MiB"))
        if e["name"].upper().endswith(".IFO"):
            ifos[e["name"]] = (fe, udf.file_bytes(fe), udf.file_bytes(fe, whole_blocks=True))
            if dump_dir:
                os.makedirs(dump_dir, exist_ok=True)
                with open(os.path.join(dump_dir, e["name"]), "wb") as fo:
                    fo.write(ifos[e["name"]][1])
    for name in sorted(ifos):
        fe, data, blocks = ifos[name]
        if name.upper() == "VIDEO_TS.IFO":
            info = parse_ifo(data, "VMG")
        elif name.upper().startswith("VTS_") and name.upper().endswith("_0.IFO"):
            info = parse_ifo(data, "VTS")
        else:
            continue
        print("IFO %s:" % name)
        for k, v in info.items():
            print("   %-28s %s" % (k, v))
    # DVDDiscID per libdvdread (w3b ref): MD5 over full IFO blocks, VIDEO_TS.IFO then VTS_nn_0.IFO for nr_title_sets+1 (cap 10)
    if "VIDEO_TS.IFO" in ifos:
        vmg = ifos["VIDEO_TS.IFO"][1]
        ntitle = u16be(vmg, 0x3E) + 1
        h = hashlib.md5()
        used = []
        for t in range(min(ntitle, 10)):
            name = "VIDEO_TS.IFO" if t == 0 else "VTS_%02d_0.IFO" % t
            if name in ifos:
                h.update(ifos[name][2])
                used.append(name)
        print("DVDDiscID-candidate (MD5 over %s): %s" % (" + ".join(used), h.hexdigest()))


def cmd_ts(path, limit):
    data = open(path, "rb").read(limit)
    if data[0] == 0x47 and data[188] == 0x47:
        pktsize, off = 188, 0
    elif len(data) > 196 and data[4] == 0x47 and data[196] == 0x47:
        pktsize, off = 192, 4
    else:
        print("no TS sync pattern"); return
    print("== TS %s read=%d bytes packet=%d tp_header=%d" % (path, len(data), pktsize, off))
    pid_count, streams = {}, {}
    psi = {}
    base = 0
    bad = 0
    while base + off + 188 <= len(data):
        ts = data[base + off: base + off + 188]
        base += pktsize
        if ts[0] != 0x47:
            bad += 1
            continue
        pid = ((ts[1] & 0x1F) << 8) | ts[2]
        pid_count[pid] = pid_count.get(pid, 0) + 1
        afc = (ts[3] >> 4) & 3
        p = 4
        if afc & 2:
            p = 5 + ts[4]
        if not afc & 1 or p >= 188:
            continue
        payload = ts[p:]
        if pid in (0, 0x11, 0x12, 0x14) or pid in streams.get("_pmt_pids", set()):
            if ts[1] & 0x40:
                ptr = payload[0]
                cur = psi.get(pid)
                if cur is not None:
                    cur.extend(payload[1:1 + ptr])
                    _psi_sections(pid, cur, streams)
                psi[pid] = bytearray(payload[1 + ptr:])
            else:
                if pid in psi:
                    psi[pid].extend(payload)
    for pid, cur in psi.items():
        _psi_sections(pid, cur, streams)
    print("packets: total_pids=%d bad_sync=%d" % (len(pid_count), bad))
    print("PIDs seen (count): " + ", ".join("0x%04X:%d" % (k, v) for k, v in sorted(pid_count.items())))
    print("PAT programs: %s" % streams.get("pat"))
    print("SDT services: %s" % streams.get("sdt"))
    print("EIT(0x12) present: %s  TDT(0x14) present: %s" % (0x12 in pid_count, 0x14 in pid_count))
    for prog in streams.get("pmt", []):
        print("PMT pid=0x%04X program=%d pcr_pid=0x%04X prog_descs=%s" % (prog["pid"], prog["program"], prog["pcr"], prog["descs"]))
        for s in prog["es"]:
            print("   ES pid=0x%04X stream_type=0x%02X (%s) descs=%s" % (s["pid"], s["type"], s["codec"], s["descs"]))


def _psi_sections(pid, buf, streams):
    off = 0
    while off + 3 <= len(buf):
        tid = buf[off]
        if tid == 0xFF:
            break
        seclen = ((buf[off + 1] & 0x0F) << 8) | buf[off + 2]
        sec = bytes(buf[off:off + 3 + seclen])
        if len(sec) < 3 + seclen:
            break
        if pid == 0 and tid == 0:
            pat = streams.setdefault("pat", [])
            for i in range(8, 3 + seclen - 4, 4):
                prog = u16be(sec, i)
                ppid = ((sec[i + 2] & 0x1F) << 8) | sec[i + 3]
                if (prog, "0x%04X" % ppid) not in pat:
                    pat.append((prog, "0x%04X" % ppid))
                if prog:
                    streams.setdefault("_pmt_pids", set()).add(ppid)
        elif tid == 2:
            pmt = {"pid": pid, "program": u16be(sec, 3), "pcr": ((sec[8] & 0x1F) << 8) | sec[9], "es": []}
            pil = ((sec[10] & 0x0F) << 8) | sec[11]
            pmt["descs"] = _descs(sec[12:12 + pil])
            i, end = 12 + pil, 3 + seclen - 4
            while i + 5 <= end:
                st, epid = sec[i], ((sec[i + 1] & 0x1F) << 8) | sec[i + 2]
                eil = ((sec[i + 3] & 0x0F) << 8) | sec[i + 4]
                pmt["es"].append({"pid": epid, "type": st, "codec": STREAM_TYPE.get(st, "?"),
                                  "descs": _descs(sec[i + 5:i + 5 + eil])})
                i += 5 + eil
            streams.setdefault("pmt", [])
            streams["pmt"] = [x for x in streams["pmt"] if x["pid"] != pid] + [pmt]
        elif tid == 0x42 and pid == 0x11:
            sdt = streams.setdefault("sdt", [])
            i, end = 11, 3 + seclen - 4
            while i + 5 <= end:
                sid = u16be(sec, i)
                dl = ((sec[i + 3] & 0x0F) << 8) | sec[i + 4]
                for tag, body in _desc_iter(sec[i + 5:i + 5 + dl]):
                    if tag == 0x48:
                        stype, plen = body[0], body[1]
                        prov = body[2:2 + plen].decode("latin-1", "replace")
                        nlen = body[2 + plen]
                        name = body[3 + plen:3 + plen + nlen].decode("latin-1", "replace")
                        rec = {"service_id": sid, "service_type": "0x%02X" % stype, "provider": prov, "name": name}
                        if rec not in sdt:
                            sdt.append(rec)
                i += 5 + dl
        off += 3 + seclen


STREAM_TYPE = {0x01: "MPEG-1 video", 0x02: "MPEG-2 video", 0x03: "MPEG-1 audio", 0x04: "MPEG-2 audio",
               0x06: "private PES (AC-3/DTS/teletext via descs)", 0x0F: "AAC ADTS", 0x11: "AAC LATM",
               0x1B: "H.264", 0x24: "H.265/HEVC", 0x81: "AC-3 (ATSC)", 0x87: "E-AC-3 (ATSC)",
               0x86: "DTS (SCTE)"}

DESC_NAMES = {0x05: "registration", 0x0A: "ISO639", 0x28: "AVC video", 0x48: "service", 0x52: "stream_identifier",
              0x56: "teletext", 0x59: "subtitling", 0x6A: "AC-3", 0x7A: "E-AC-3", 0x7C: "AAC", 0x1B: "MPEG-4 video",
              0x2B: "MPEG-4 audio", 0x7F: "extension", 0x69: "scrambling"}


def _desc_iter(b):
    i = 0
    while i + 2 <= len(b):
        tag, ln = b[i], b[i + 1]
        yield tag, b[i + 2:i + 2 + ln]
        i += 2 + ln


def _descs(b):
    out = []
    for tag, body in _desc_iter(b):
        name = DESC_NAMES.get(tag, "0x%02X" % tag)
        if tag == 0x0A:  # ISO639: 3-char language + audio type
            out.append("%s:lang=%s type=0x%02X" % (name, body[:3].decode("latin-1", "replace"), body[3] if len(body) > 3 else 0))
        elif tag == 0x05:
            out.append("%s:format=%r" % (name, body[:4].decode("latin-1", "replace")))
        elif tag == 0x59:  # subtitling: 3 lang + type + comp/aux
            out.append("%s:lang=%s type=0x%02X" % (name, body[:3].decode("latin-1", "replace"), body[3]))
        else:
            out.append("%s(len=%d)" % (name, len(body)))
    return out


def cmd_avi(path, limit):
    data = open(path, "rb").read(limit)
    if data[:4] != b"RIFF" or data[8:12] != b"AVI ":
        print("not RIFF AVI"); return
    print("== AVI %s read=%d bytes riff_size=%d" % (path, len(data), u32le(data, 4)))
    streams, info, odml = [], {}, {}
    state = {"stream": None, "movi": None, "first_video": None, "idx1": False, "ix": 0}
    _avi_walk(data, 12, len(data), [], streams, info, odml, state)
    for s in streams:
        print("  stream %d: %s" % (s["index"], s))
    print("INFO: %s" % info)
    print("odml: %s" % odml)
    print("movi offset=%s idx1_in_read=%s ix_chunks_in_movi=%d" % (state["movi"], state["idx1"], state["ix"]))
    fv = state["first_video"]
    if fv:
        off, size = fv
        chunk = data[off:off + min(size, 4096)]
        print("first video chunk off=%d size=%d" % (off, size))
        for key in (b"Lavc", b"XviD", b"DivX", b"x264", b"Lavf"):
            pos = data.find(key, 0, len(data))
            if pos >= 0:
                print("  found %r at file offset %d: %r" % (key, pos, data[pos:pos + 40]))
        print("  first 32 bytes of chunk payload: %r" % chunk[:32])


def _avi_walk(data, pos, end, path, streams, info, odml, state):
    while pos + 8 <= end:
        cid = data[pos:pos + 4]
        size = u32le(data, pos + 4)
        body = pos + 8
        if cid in (b"RIFF", b"LIST"):
            typ = data[body:body + 4]
            if typ == b"movi":
                state["movi"] = pos
            _avi_walk(data, body + 4, min(end, body + size), path + [typ.decode("latin-1")], streams, info, odml, state)
        else:
            cur = body + min(size, end - body)
            if cid == b"avih":
                us, mbps, _pad, flags, tot, init, nstr, bufs, w, h = struct.unpack_from("<10I", data, body)
                print("avih: us/frame=%d (%.3f fps) max_bytes/s=%d flags=0x%X total_frames=%d streams=%d %dx%d" % (
                    us, 1e6 / us if us else 0, mbps, flags, tot, nstr, w, h))
            elif cid == b"strh":
                fcc, handler, _f, _p, lang, _init, scale, rate, _st, length = struct.unpack_from("<4s4sIHHIIIII", data, body)
                state["stream"] = {"fcc": fcc.decode("latin-1"), "handler": handler.decode("latin-1") if handler.isascii() else handler.hex(),
                                   "scale": scale, "rate": rate, "length": length, "lang": lang, "index": len(streams)}
                streams.append(state["stream"])
            elif cid == b"strf" and state["stream"]:
                s = state["stream"]
                if s["fcc"] == "vids":
                    sz, w, h, planes, bits, comp = struct.unpack_from("<IiiHH4s", data, body)
                    s.update({"width": w, "height": h, "bitcount": bits, "compression": comp.decode("latin-1")})
                elif s["fcc"] == "auds":
                    tag, ch, sr, abps, align, bps = struct.unpack_from("<HHIIHH", data, body)
                    s.update({"wFormatTag": "0x%04X" % tag, "channels": ch, "samples_per_sec": sr,
                              "avg_bytes_per_sec": abps, "block_align": align, "bits": bps})
                    cbsize = u16le(data, body + 16) if size >= 18 else 0
                    s["cbSize"] = cbsize
                    s["bitrate_kbps_from_avg"] = abps * 8 // 1000
            elif cid == b"strn" and state["stream"]:
                state["stream"]["name"] = data[body:cur].split(b"\x00", 1)[0].decode("latin-1", "replace")
            elif cid == b"dmlh":
                odml["total_frames"] = u32le(data, body)
            elif cid == b"indx" and state["stream"]:
                odml.setdefault("indx", []).append({"stream": state["stream"]["index"], "entries_in_use": u32le(data, body + 4), "chunk_id": data[body + 8:body + 12].decode("latin-1")})
            elif cid == b"JUNK":
                info.setdefault("JUNK", []).append({"size": size, "head": data[body:body + 16]})
            elif cid == b"idx1":
                state["idx1"] = True
            elif cid == b"ix00" and "movi" in path:
                state["ix"] += 1
            if cid in (b"00dc", b"00db", b"01dc", b"01db") and state["first_video"] is None and cid[2:4] in (b"dc", b"db"):
                state["first_video"] = (body, size)
            if len(path) and path[-1] == "INFO" and cid != b"LIST":
                info[cid.decode("latin-1")] = data[body:cur].split(b"\x00", 1)[0].decode("latin-1", "replace")
        pos = body + size + (size & 1)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)
    cmd, target = sys.argv[1], sys.argv[2]
    rest = sys.argv[3:]
    lim = None
    if "--limit" in rest:
        lim = int(rest[rest.index("--limit") + 1])
    if cmd == "iso":
        dd = rest[rest.index("--dump-dir") + 1] if "--dump-dir" in rest else None
        cmd_iso(target, dd)
    elif cmd == "ts":
        cmd_ts(target, lim or 700 * KB)
    elif cmd == "avi":
        cmd_avi(target, lim or 4 * 1024 * KB)
    else:
        print(__doc__)
        sys.exit(1)
