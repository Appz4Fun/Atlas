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
import socketserver
import struct
import time

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


def build_posts():
    # dated a few hours back so post-processing's settle time has passed
    when = email.utils.formatdate(time.time() - 4 * 3600)
    posts = []

    def add(subject, body, author="poster <p@mock.example>", size=None):
        n = len(posts) + 1
        raw = sum(len(l) + 2 for l in body)
        posts.append(dict(n=n, subject=subject, author=author, date=when,
                          msgid=f"<msg{n}@mock.example>", bytes=size or raw, body=body))

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
                    for h in (f"From: {p['author']}", f"Subject: {p['subject']}", f"Message-ID: {arg}",
                              f"Newsgroups: {GROUP}", "User-Agent: Nyuu/0.4.2", ""):
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
