#!/usr/bin/env python3
"""A tiny plaintext NNTP server with a handful of realistic binary posts.

Atlas indexes it like a real provider (GROUP / XOVER / BODY / STAT / ARTICLE),
so a fresh ATLAS_HOME ends up with real releases to search, browse and pull
NZBs for, without a Usenet account.

    python3 mock_nntp.py --port 1119            # serves until killed
    user: any, password: secret, group: alt.binaries.test

Stdlib only. Mirrors tests/common/mod.rs (the Rust mock the test suite uses).
"""
import argparse
import email.utils
import io
import random
import socketserver
import struct
import tarfile
import time
import zipfile
import zlib

GROUP = "alt.binaries.test"
PASSWORD = "secret"


def yenc(name, data, part=None, total=None, begin=1):
    def enc(chunk):
        out = bytearray()
        for b in chunk:
            e = (b + 42) & 0xFF
            if e in (0, 10, 13, 61):
                out += bytes([61, (e + 64) & 0xFF])
            else:
                out.append(e)
        return bytes(out)

    head = f"=ybegin line=128 size={len(data)} name={name}".encode()
    lines = [head]
    lines += [enc(data[i:i + 100]) for i in range(0, len(data), 100)]
    lines.append(f"=yend size={len(data)}".encode())
    return lines


def par2_filedesc(name, size):
    nb = name.encode()
    nb += b"\0" * (-len(nb) % 4)
    length = 64 + 56 + len(nb)
    p = b"PAR2\0PKT" + struct.pack("<Q", length) + b"\0" * 32
    p += b"PAR 2.0\0FileDesc" + b"\7" * 48 + struct.pack("<Q", size) + nb
    return p


def nested_archive(name):
    """A RAR5 (stored) holding a 7z (copy) holding a zip (deflate) holding a tar holding a Matroska file."""
    rng = random.Random(5)
    mkv = b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\xf7\x81\x01\x42\xf2\x81\x04\x42\xf3\x81\x08\x42\x82\x88matroska"
    mkv += bytes(rng.getrandbits(8) for _ in range(20_000))
    t = io.BytesIO()
    with tarfile.open(fileobj=t, mode="w", format=tarfile.USTAR_FORMAT) as tf:
        info = tarfile.TarInfo(f"{name}.mkv")
        info.size = len(mkv)
        tf.addfile(info, io.BytesIO(mkv))
    z = io.BytesIO()
    with zipfile.ZipFile(z, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(f"{name}.tar", t.getvalue())
    packed, nxt = z.getvalue(), b"\0" * 8
    start = struct.pack("<QQI", len(packed), len(nxt), zlib.crc32(nxt))
    sz = b"7z\xbc\xaf\x27\x1c\x00\x04" + struct.pack("<I", zlib.crc32(start)) + start + packed + nxt

    def vint(n):
        out = bytearray()
        while True:
            b, n = n & 0x7F, n >> 7
            out.append(b | (0x80 if n else 0))
            if not n:
                return bytes(out)

    def block(ty, fields, data=None):
        inner = vint(ty) + vint(2 if data is not None else 0) + (vint(len(data)) if data is not None else b"") + fields
        body = vint(len(inner)) + inner
        return struct.pack("<I", zlib.crc32(body)) + body + (data or b"")

    fname = f"{name}.7z".encode()
    fields = vint(0) + vint(len(sz)) + vint(0x20) + vint(0) + vint(0) + vint(len(fname)) + fname
    return b"Rar!\x1a\x07\x01\x00" + block(1, vint(0)) + block(2, fields, sz) + block(5, vint(0))


def build_posts():
    # dated a few hours back so post-processing's settle time has passed; GMT, as Nyuu writes it
    posted = time.time() - 4 * 3600
    when = email.utils.formatdate(posted, usegmt=True)
    posts = []

    def nyuu_id(n):
        # Nyuu's grammar: 24 letters in alternating case, a dash, the millisecond clock
        letters = "".join(chr((65 if i % 2 == 0 else 97) + (n * 7 + i * 5) % 26) for i in range(24))
        return f"<{letters}-{int(posted * 1000) + n * 1500}@nyuu>"

    def add(subject, body, author="poster <p@mock.example>", size=None):
        n = len(posts) + 1
        raw = sum(len(l) + 2 for l in body)
        posts.append(dict(n=n, subject=subject, author=author, date=when,
                          msgid=nyuu_id(n), bytes=size or raw, body=body))

    # a movie: 2 rar volumes (2 articles each), an nfo and a par2 index
    movie = "Cool.Movie.2024.1080p.WEB-DL.DDP5.1.H.264-GRP"
    for vol in (1, 2):
        for seg in (1, 2):
            add(f'[{vol}/4] - "{movie}.part{vol}.rar" yEnc ({seg}/2)',
                yenc(f"{movie}.part{vol}.rar", bytes([vol * seg]) * 3000), size=750_000)
    add(f'[3/4] - "{movie}.nfo" yEnc (1/1)', yenc(f"{movie}.nfo", f"{movie}\nVideo: H.264\n".encode()))
    par = par2_filedesc(f"{movie}.part1.rar", 1_500_000) + par2_filedesc(f"{movie}.part2.rar", 1_500_000)
    add(f'[4/4] - "{movie}.par2" yEnc (1/1)', yenc(f"{movie}.par2", par))

    # a tv episode, single mkv in 3 articles
    ep = "Some.Show.S02E05.720p.HDTV.x264-TVG"
    for seg in (1, 2, 3):
        add(f'"{ep}.mkv" yEnc ({seg}/3)', yenc(f"{ep}.mkv", b"\x1a\x45\xdf\xa3" * 500), size=600_000,
            author="tvposter <tv@mock.example>")

    # an album, two flac tracks
    alb = "Artist-Album-2023-FLAC-MUS"
    for i, t in enumerate(("01-artist-intro.flac", "02-artist-song.flac"), 1):
        add(f'[{i}/2] - "{alb}" - "{t}" yEnc (1/1)', yenc(t, b"fLaC" + b"\0" * 2000), size=300_000)

    # an obfuscated upload, named only by its par2
    obf = "a1B2c3D4e5F6g7H8i9J0"
    add(f'[1/2] - "{obf}.part1.rar" yEnc (1/1)', yenc(f"{obf}.part1.rar", b"x" * 4000), size=2_000_000)
    add(f'[2/2] - "{obf}.par2" yEnc (1/1)',
        yenc(f"{obf}.par2", par2_filedesc("Hidden.Name.2022.2160p.UHD.BluRay.x265-HDR.mkv", 2_000_000)))

    # archives inside archives: RAR5 > 7z > zip > tar > mkv
    nest = "Nested.Movie.2021.1080p.BluRay.x264-NEST"
    data = nested_archive(nest)
    for seg in (1, 2, 3):
        add(f'"{nest}.rar" yEnc ({seg}/3)', yenc(f"{nest}.rar", data), size=900_000,
            author="nester <n@mock.example>")
    return posts


POSTS = build_posts()


class Handler(socketserver.StreamRequestHandler):
    def send(self, s):
        self.wfile.write((s if isinstance(s, bytes) else s.encode()) + b"\r\n")

    def send_body(self, lines):
        for l in lines:
            self.send(b"." + l if l.startswith(b".") else l)
        self.send(".")

    def find(self, msgid):
        return next((p for p in POSTS if p["msgid"] == msgid), None)

    def handle(self):
        self.send("200 atlas mock news server ready")
        selected = None
        for raw in self.rfile:
            cmd = raw.decode("utf-8", "replace").rstrip("\r\n")
            verb, _, arg = cmd.partition(" ")
            verb = verb.upper()
            if verb == "AUTHINFO" and arg.upper().startswith("USER"):
                self.send("381 more")
            elif verb == "AUTHINFO":
                self.send("281 ok" if arg == f"PASS {PASSWORD}" else "481 nope")
            elif verb == "CAPABILITIES":
                self.send("101 caps")
                for c in ("VERSION 2", "READER", "OVER", "AUTHINFO USER", "."):
                    self.send(c)
            elif verb == "MODE":
                self.send("200 reader")
            elif verb == "DATE":
                self.send("111 " + time.strftime("%Y%m%d%H%M%S", time.gmtime()))
            elif verb == "GROUP":
                if arg == GROUP:
                    selected = arg
                    self.send(f"211 {len(POSTS)} 1 {len(POSTS)} {arg}")
                else:
                    self.send("411 no such group")
            elif verb == "LIST":
                self.send("215 list follows")
                self.send(f"{GROUP} {len(POSTS)} 1 y")
                self.send(".")
            elif verb in ("XOVER", "OVER"):
                if not selected:
                    self.send("412 no newsgroup selected")
                    continue
                a, _, b = arg.partition("-")
                a = int(a or 1)
                b = int(b) if b else len(POSTS)
                hits = [p for p in POSTS if a <= p["n"] <= b]
                if not hits:
                    self.send("423 no articles in that range")
                    continue
                self.send("224 overview follows")
                for p in hits:
                    self.send(f'{p["n"]}\t{p["subject"]}\t{p["author"]}\t{p["date"]}\t{p["msgid"]}\t\t{p["bytes"]}\t10')
                self.send(".")
            elif verb == "STAT":
                self.send("223 0 there" if self.find(arg) else "430 no such article")
            elif verb in ("BODY", "ARTICLE"):
                p = self.find(arg)
                if not p:
                    self.send("430 no such article")
                    continue
                if verb == "ARTICLE":
                    self.send(f"220 0 {arg}")
                    # the headers a provider adds on injection, so the release page has them to show
                    for h in (f"Path: news.mock.example!feeder.mock.example!not-for-mail",
                              f"From: {p['author']}", f"Subject: {p['subject']}", f"Message-ID: {arg}",
                              f"Newsgroups: {GROUP}", f"Date: {p['date']}", f"Injection-Date: {p['date']}",
                              'Injection-Info: news.mock.example; posting-account="u-1234"; '
                              'posting-host="203.0.113.7"; logging-data="4242"; mail-complaints-to="abuse@mock.example"',
                              "Organization: Mock Usenet", "User-Agent: Nyuu/0.4.2", ""):
                        self.send(h)
                else:
                    self.send(f"222 0 {arg}")
                self.send_body(p["body"])
            elif verb == "QUIT":
                self.send("205 bye")
                return
            else:
                self.send("500 what")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=1119)
    port = ap.parse_args().port
    with Server(("127.0.0.1", port), Handler) as s:
        print(f"mock nntp on 127.0.0.1:{port}, group {GROUP}, {len(POSTS)} posts", flush=True)
        s.serve_forever()
