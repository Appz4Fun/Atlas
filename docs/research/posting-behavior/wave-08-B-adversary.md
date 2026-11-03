# Adversary view: what uploaders defeat vs. what they leak (w8b)

Method: read the ngPost, Nyuu, postie and nxg-upper source (cloned 2026-11-02) plus forums. Public advice covers names and reposts, not metadata.

| Signal | Uploaders know? (cite) | Option that defeats it | Default | Residual leak when defeated |
|---|---|---|---|---|
| Subject / filename | Yes [ngP][pos][hh] | ngPost `obfuscate = article` / `-x` (subject = UUID); postie `obfuscation_policy: full`; nxg `Obfuscate`; Nyuu `--subject` with `${rand(N)}` | ngPost off, postie **full**, nxg off, Nyuu off | ngPost: subject == Message-ID local part, which is a tool signature. **postie `partial`**: subject = md5(original subject) and yEnc name = md5(filename+part). Both are deterministic, so they can be dictionary-reversed from predb names |
| yEnc `name=` | Partly [pos] | postie full ("randomized for every article"); nxg `ObfuscateYenc`; Nyuu `--yenc-name` | ngPost cannot change it (it writes the real or RAR name, and `--gen_name` only renames the archive) | No tool touches them: `size=`, `total=`, `part=` and `=ypart` stay true. `line=128` is the default in all 4 tools (postie through rapidyenc), so it doesn't tell tools apart |
| From / poster | Yes [ngP][pos][hh] | ngPost `GEN_FROM`/random; postie full = random per article; nxg `ObfuscatePoster` | ngPost: one random `<13 chars>@ngPost.com` per session. The sample conf `GEN_FROM = true` makes it one per post, and obfuscation makes it one per article. **Nyuu: real `$USER <$USER@hostname>`**. nxg: one random `rand12@rand8.rand3` per upload | ngPost From domain = msg_id signature + ".com". postie format `rand14 <rand14@rand5.rand3>`; nxg `hex15@hex15.hex3`. The shapes are tool-specific |
| Message-ID format | Barely. ngPost exposes `msg_id`, Nyuu `--message-id` | ngPost `msg_id`/`-z`; Nyuu `--message-id`; postie `message_id_format` | ngPost `<32hex>@ngPost`; **Nyuu `AbCd…(24 alternating case)-<13-digit ms timestamp>@nyuu`**; postie `rand32@rand8.rand3`; nxg `sha256[0:40]@[40:61].[61:64]` | The local-part grammar survives a domain change. Nyuu's timestamp gives per-article encode time and order across groups |
| User-Agent / X-Newsposter / extra headers | Rarely. Nyuu docs mention stripping UA only for timeouts | Nyuu `-H User-Agent` (unset); postie `custom_headers` | Nyuu sends `User-Agent: Nyuu/x.y`; ngPost sends none; nxg sends `X-Nxg`; postie can add `X-Nxg` | Header **set/order** is a fingerprint: ngPost writes only From/Newsgroups/Subject/Message-ID. postie writes headers from a Go map, so their order is random per article |
| Date header | Yes, postie and Nyuu only [pos][nyuu] | Nyuu `--date`; postie full ("randomized within last 6 hours") | Real time otherwise; ngPost omits Date so the server adds it | Server `NNTP-Posting-Date`/`Injection-Date`, article numbers and Xref are not controllable. Nyuu `--date` makes the msgid timestamp **constant** across the run, which links the articles |
| Posting order / cadence | Never mentioned. Web searches found no advice on jitter | None. No tool shuffles or jitters | Files go out in order, interleaved over N parallel connections | Contiguous article numbers. A release is one dense time window across groups |
| Group spraying | Yes [ngP][pos] | ngPost `GROUP_POLICY = EACH_FILE` (only with obfuscation) / `EACH_POST`; postie `group_policy: each_file` | ngPost EACH_POST; postie each_file | One file stays in one group. Same time window, same server numbering, same yEnc size |
| Article size | Never | `article_size` / `-a` / `article_size_in_bytes` / `ArticleSize` | ngPost 716800, Nyuu 700K (716800), postie 750000, nxg 768000 | The size points to the tool. Last part of each file = remainder |
| PAR2 creator / settings | Never | None found. ParPar has `--comment`, but no creator option | Creator: CLI "ParPar v<ver> <arch> [github url]" (leaks CPU arch), "Created by par2cmdline version x.", postie "Postie". None are configurable | Every PAR2 vol carries the same Recovery Set ID. FileDesc holds each file's first-16 KiB MD5, full MD5 and length (SABnzbd `par2file.py` parses these). |
| RAR/7z volume size | Never | `RAR_SIZE` (ngPost 42 MB); nxg `VolumeSize` 153600000 (multiple of ArticleSize) | on | Equal-size volumes and the count fall straight out of yEnc `size=` |
| Archive password | Yes [ngP][nxg] | `--gen_pass`, `RAR_PASS`, nxg random 25 chars, `-mhe=on` | ngPost off, nxg on | RAR5 encrypted headers still expose the volume structure/size. Without `-hp` the inner names are visible |
| NZB withholding | Yes ("you won't find your post if you lose the nzb") [ngP] | `NZB_POST_CMD` / `NZB_UPLOAD_URL` to private indexers; nxg replaces the NZB with X-Nxg | — | Articles still sit in public groups |
| Reposting after takedown | Yes ("you need automation") [r1] | postie `post_check.max_reposts`; ngPost `nzbCheck`; Nyuu check-* | postie on; ngPost `nzbCheck = false`; Nyuu `check-connections: 0` (off) | postie reposts with the **same Message-ID and headers** byte for byte. ngPost retries with a new UUID. Nyuu draws a new ID unless `--keep-message-id` is set. In every case the yEnc/PAR2 content is identical |
| Multi-server spread | Yes [r1] ("all servers get the same takedowns") | multiple `-S` servers | — | The injecting server shows in Path/Xref |
| NNTP-Posting-Host / X-Trace | Yes ("your provider inserts header lines") [hh]. Header dumps in [ut] still show them | None (VPN only) | Server-side | Per-provider/account hash. Stable across a session if not stripped |

## Blind spots for Atlas (most reliable first)
1. **PAR2 Recovery Set ID + 16 KiB MD5 + file length matched to yEnc `size=`.** Repair needs them, so no tool can change them. They hold across groups, posters and subjects ([r/newznab] users are "hitting a dead wall").
2. **yEnc `size=`/`total=` equality and volume grammar.** Not changed even by postie `full`. Equal volumes, one remainder volume and the PAR2 vol sizes cluster a release.
3. **Server timing / article-number contiguity.** No tool shuffles or jitters. postie's Date randomization doesn't reach server dates or Xref.
4. **Tool grammar in Message-ID/From/header-set/article size.** Nyuu msgid timestamps give exact ms ordering (or a constant value under `--date`).
5. **Repost identity.** Reposts carry identical yEnc/PAR2 bytes, and postie also keeps the same Message-IDs, so a takedown and its repost link to each other.
6. **Habit/default leaks.** The PAR2 creator (tool + version + arch), slice size and redundancy % are a stable per-uploader fingerprint across releases. Nyuu's default `From` (real user@host) and `User-Agent: Nyuu/ver`, ngPost `@ngPost.com` From.
7. **ngPost PRNG (needs testing).** The random From, RAR name, password and EACH_FILE group all come from one libc `std::rand()` stream, seeded by `srand(QUuid().data1)` (32 bits, NgPost.cpp:315). Brute-forcing the 2^32 seed from public From strings may recover the archive name and password. Not tested.

Sources:
[ngP] https://github.com/mbruel/ngPost (README, ngPost.conf, src/nntp/NntpArticle.cpp)
[nyuu] https://github.com/animetosho/Nyuu (help-full.txt, config.js, lib/article.js)
[pos] https://github.com/javi11/postie (docs/docs/obfuscation.md, config-example.yaml, internal/article/article.go, internal/par2/par2.go)
[nxg] https://github.com/Tensai75/nxg-upper (README, defaultconfig.go, postArticle.go)
ParPar: https://raw.githubusercontent.com/animetosho/ParPar/master/help-full.txt · lib/par2gen.js · par2cmdline src/creatorpacket.cpp: https://github.com/Parchive/par2cmdline
SABnzbd PAR2 parser: https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/par2file.py
[hh] https://harley.com/usenet/file-sharing/05-anonymous-file-sharing.html
[r1] https://pol1.lr.ggtyler.dev/r/usenet/comments/1jvtt28/best_usenet_backbones_coverage_to_avoid_dmca
[r/newznab] https://pol1.lr.ggtyler.dev/r/newznab/comments/18pfnp7/newznab_deobfuscation
[ut] https://redlib.ssps.io/r/UsenetTalk/comments/63lapa/new_usbased_tier_1_usenet_service_provider/dg30b8t/
newsbin: https://forums2.newsbin.com/viewtopic.php?p=221348 · SABnzbd: https://forums.sabnzbd.org/viewtopic.php?p=122040
