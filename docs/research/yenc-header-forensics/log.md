# yEnc and NNTP header forensics research log

Started 2026-11-02 06:45Z. 10 waves x 3 Haiku agents (medium, 15 min cap), Opus main session merges and steers.
Goal: the most obscure yEnc/NNTP header signals that link the parts of one binary post (and its twins) together.

## Wave 1 (06:45Z)
- A: yEnc spec and encoder quirks (=ybegin/=ypart/=yend fields, line length, escaping, CRC habits)
- B: Message-ID formats and posting-client fingerprints
- C: NNTP transport headers (Path, Xref, NNTP-Posting-Host, Injection-Info, header order, Date)

Sharing rule (06:45Z): after each wave, message_thread a short summary to the other 9 research threads and append best leads to docs/research/linking-findings.md under "Haiku 15-min research: yEnc headers".
Peers: catalog QRvGqGQ11Ug8Fy3joSUAvf; Opus NjSX8S5dKnseDjkXiz1L45, VAsQ842SMP5j5rPUMk322k, J9gfzLaZtXQCsj3gDW1s1Z; Haiku10 32R21cxmxbYS5gZFaqvbbZ, RNbj2ULJGyZGBjyy2rVd3F, H9w4Fv7jgdKWEeCawC3t8S; Haiku15 L1M1cF5Q6PLij2VJnxMJSG, BQpoS35Uct66oBKpWbtLk8 (all prefixed cmsg_014FRqgd95UUF8Mvov1QRBWr)

### W1A result (yEnc spec quirks)
- ybegin size=/name= shared across parts; yend size= is part size. ypart begin/end 1-based contiguous ranges reveal split size; begin=0 marks off-by-one encoders.
- pcrc32 (per part, required multipart) is the part-level twin key, independent of line length/escaping. crc32 whole-file usually last part only.
- Field order free except name= last; tab-before-name exists in the wild (yencoding crate comment). Hex case/padding, line= value, escape set (extra TAB/SPACE/DOT escaping, leading-dot doubling) are per-encoder fingerprints. Over-escaping is decoder-invisible, only seen by escape-frequency stats.
- Missing total= = legacy encoder.
- Caveat: per-tool mapping UNVERIFIED; normalise by tokens not header text.
- Sources: yence.sourceforge.net 1.3 draft; docs.rs yencoding header.rs.

### W1C result (NNTP transport)
- Xref: same Message-ID with multiple group:num on one server = true crosspost (one article). Numbers per-server only; matching numbers across servers suggests INN xrefslave. Xref is an overview extra, probe LIST OVERVIEW.FMT.
- Path first token = injecting host; Injection-Info logging-data is a per-session id (groups a batch, not a poster); Injection-Date minus Date = per-injector/client clock fingerprint.
- Date deltas + fixed tz offset / wrong clock chain a batch. :bytes/:lines stable per segment.
- Header order fingerprint: hypothesis only, untested. Provider stripping (Omicron/Abavia/UE/Eweka) UNVERIFIED -> needs a live HEAD test.
- Sources: GIAC usenet forensics primer; news.software.nntp INN FAQ thread; IETF usefor list; RFC 3977.
- Note: W1A finished in 45s. Agents under-use the 15 min; push them harder to read source code.

### W1B result (Message-ID / client)
- Nyuu default Message-ID ends `-<unix-ms>@nyuu`: per-article ms timestamp clusters a run even with random subject/From (PR #148). Default From login@hostname, UA Nyuu/<ver>. Nyuu needs fixed-length IDs; `${rand(N)}` templates.
- Nyuu-Obfuscation fork: Message-ID `{rand(32)}@{rand(8)}.{rand(3)}`, From `{rand(14)} <{rand(14)}@{rand(5)}.{rand(3)}>`, random yEnc name + Date per article, no UA. Shape of the random ID is itself a fingerprint.
- NewsUP default `NewsUP <NewsUP@somewhere.cbr>`, generate_ids toggle, X-extra-header. ngPost format UNVERIFIED (ArticleBuilder.cpp).
- Stock random IDs give no shared prefix across parts.

### Wave 1 merge
Strongest: (1) yEnc ybegin size+name and ypart contiguous begin/end, (2) pcrc32 per part + crc32 whole-file as twin keys, (3) Nyuu ms-timestamp Message-ID suffix, (4) Xref same-ID multi-group = crosspost, (5) Injection-Date minus Date and Date deltas as batch fingerprint.
Gaps: per-tool fingerprint table (line=, escape set, hex case, field order, header order) unverified; provider header stripping unverified.
Decision: LEAN IN with source-code reads for wave 2 (Nyuu lib/, ngPost ArticleBuilder, node-yencode/rapidyenc, NZBGet/SABnzbd malformed-yEnc comments), plus an obscure pivot.

### Peer input after wave 1 (06:47Z)
- Haiku10 yEnc thread: newsmangler Message-ID <unixtime.5dp.part@server-host> (no randomness, orderable from OVER); subject zero-pad width per batch; Injection-Info posting-account per account.
- Posting-behavior threads: Nyuu ID rand24-{ts}@nyuu re-rolled per repost (never a twin key); ngPost uuid@ngPost, part 716800 vs Nyuu 700K, ngPost GROUP_POLICY one-group-per-file (Batman split cause); ngPost -x Subject==MsgID local; Nyuu --date now shares one Date; kkm/nzb-indexer 4h incomplete hold. Subagents can't use GitHub API, use raw.githubusercontent.com. They are already reading Nyuu/ngPost/nxg-upper source and nZEDb collectionhash -> AVOID duplicating; my wave 2 should go to decoders (rapidyenc/sabyenc/nzbget/node-yencode), older Windows posters (PowerPost, JBinUp, Camelsystem, yEnc32, Usenet Uploader), and an obscure pivot.
- Payload thread: whole-file CRC32 = crc32fast combine over pcrc32 + ypart lengths (lets us verify yEnc crc32= vs computed, and derive file CRC when crc32= missing). PAR2 RSID is name-bound. They cover PAR2/RAR/srrDB.
- Catalog thread: X-Received-Body-CRC header (server-computed body CRC? investigate in wave 2), Xref:full in XOVER col 9. Catalog artifact https://claude.ai/artifact/PJsrDryQJnLBCvxKmAjLaX
- Opus yEnc thread W1: ngPost body fingerprint (unpadded pcrc32, =yend no part=/crc32=, trailing TAB/SPACE unescaped, empty line before =yend at 128-multiples; fixed in Hydro74000 fork v5.6 = version marker). GoPostStuff uppercase %08X, UnixNano$gps@gopoststuff. Rust yenc crate omits total=; python-yenc dot-stuffs. Nyuu ID ^([A-Z][a-z]){12}-\d{13}@nyuu$ ms==Date. newsmangler leaks provider host. ngPost sends no Date. Use part size + :lines not :bytes. Their W2: PRNG state recovery, live backbone headers, older poster grammars -> I avoid those; my W2 = decoders' workaround lists, X-Received-Body-CRC/provider-added headers via docs/forums, and obscure pivot (archive.org/IRC/predb-era posting rules).
- Opus posting-behavior W1: ngPost --obfuscate random From share Message-ID domain; arrival-curve join across groups; DMCA co-removal via STAT over time (rp.os3.nl/2014-2015/p82/report.pdf).

## Wave 2 (07:17Z)
- A: decoder source workaround lists (sabyenc, nzbget, rapidyenc/node-yencode, Pan, Go/Rust decoders)
- B: provider-injected headers from real HEAD dumps (X-Received-Body-CRC, X-Trace, Path tokens per backbone)
- C: obscure threading headers (References in binaries, Cancel-Lock, header order/casing, Lines)
- Haiku10 yEnc W2: X-Received-Body-CRC/Bytes undefined publicly (needs live HEAD test on two reposts). Date string tz form clusters tools (-0000 python formatdate, +0000, GMT, invalid UTC). PowerPost subjects lack 'yEnc'. NZB writer fingerprint (iso-8859-1 newzBin DOCTYPE vs Nyuu utf-8); shared password meta = same uploader.
- Opus yEnc W2: Nyuu-Obfuscation PRNG (V8 xorshift128+) state recovery from one article could predict session's other Message-IDs (unverified); ngPost 32-bit seeded std::rand; NxG 3 SHA-256 self-checks; NewsUP fakes 7 Message-ID grammars but yEnc layer reveals it; postie vs Nyuu-Obf split by Date suffix/header order/name charset. X-Received-Body-CRC needs live check (3 threads agree) -> final report should recommend a live HEAD test on Eweka via Atlas.
- Opus posting W2: backbone-overwritten IDs <hex$0$n$hex@host> (hex unix time first) -> provider-assigned ID grammar; nzbrefresh reposts same Message-ID; nzb-repair fills part>1 with begin=1 (quirk!).
- 07:27Z the owner shared "Linking signals map" (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE) as the output format example: signals by what they prove (twin/alt/same film/same uploader/posting tool) x read cost (overview row .. every article), Atlas coverage, posting-tool fingerprint table (Nyuu, ngPost, Postie, NewsUP, GoPostStuff, nxg-upper). Final report should follow this shape and fill gaps. CONFLICT: map says Nyuu segment 716,800, posting thread W1 said 700K -> verify in wave 3 from Nyuu help-full.txt.

### W2A result (decoder workarounds) - sabyenc/nzbget/rapidyenc sources mostly failed to load ("I Challenge Thee" placeholders); used Rust crates + Plan 9 ydec.c + Go nntp
- Fingerprint tuple from yEnc lines: field order, tab vs space before name=, CRC key set (pcrc32 / crc32 / bare crc=), hex padding/case/0x, size= present on =yend, total= present, begin=0, TAB/dot escaping at column 0, SPACE escaping, NNTP dot-stuffing vs yEnc-escaping leading dot, trailing "=" at line end (never emitted by normal splitters; before =yend = truncated/patched), bare CR/LF/NUL in body, preamble before =ybegin, unknown extra keys (log all key names+order).
- name= keeps internal spaces/= (e.g. name=-=DUMMY=- abc.par); anchor on "name=", never split on "=".
- yencoding encoder order: multi =ybegin part total line size name; =yend size part pcrc32 crc32.
- Decoder bug to rule out: yenc 0.2.2 never resets hasher -> false pcrc32 mismatches on parts 2+. Decoders disagree (Plan 9 vs yencoding) on missing =ypart, bare CR.
- Follow-up: fetch sabctools/nzbget via sources.debian.org or jsdelivr npm tarball; build a quirk fixture corpus.

### W2B result (provider headers)
- One real X-Received-Body-CRC sample: Usenet.Farm, alt.binaries.newznzb.bravo 2018: "X-Received-Bytes: 6481", "X-Received-Body-CRC: 2720885501" (decimal uint32, input unverified), Path just "not-for-mail", Date "Thu, 28 Jun 18 15:25:10 UTC" (2-digit year + invalid UTC zone!), Message-Id @PRiVATE. https://leo.leung.xyz/wiki/Usenet
- Highwinds Path injection token "fxNN.iad.POSTED!not-for-mail" (post02.iad.highwinds-media.com) - candidate backbone fingerprint. Farm strips Path to not-for-mail.
- Injection-Info path-identity should equal Path's injecting token; eternal-september posting-account is salted, rotates every 24h (not stable). X-Trace legacy, INN no longer emits.
- Bytes: INN counts whole article incl headers -> not a body key.
- Newsbin dup detector: RIPEMD over first 24K (claim).
- No Omicron/Eweka/Easynews dumps found -> live HEAD test still needed.

### W2C result (obscure threading headers)
- Mainstream posters set no References/Cancel-Lock. Pan list claims yencee sets References to previous part (unverified).
- NxG scheme (nxg-upper, javi11/nxg, postie message_id_format=nxg / add_nxg_header): NXG header = base64("<21char rand>:<data count>:<par2 count>"); Message-ID_n = sha256("NXGHEADER:type:n") hex split 40@21.3. Every part ID derivable from one header -> STAT the rest. X-NXG per-article presence unverified.
- postie obfuscation "partial": same poster + same yEnc filename on all articles, real Date (strong link); "full": random per article, Date +-6h.
- nZEDb collection hash = sha1(clean subject + From + group + file count); random From splits collections (Atlas should not copy this).
- Cancel-Lock RFC 8315: HMAC(secret, uid+mid) -> per-article different, not linkable. Thunderbird old Message-ID hex-time.salt@domain.

### Wave 2 merge
Leaning: yEnc layer fingerprint tuple is rich and well-sourced; X-Received-Body-CRC is the one potential free HEAD twin key but unverified (1 sample, Usenet.Farm); NxG derivable IDs are the strongest obscure link.
Wave 3 plan: A nxg/postie/Nyuu source via jsdelivr/npm or raw (confirm NxG derivation + X-NXG, Nyuu exact yEnc output + segment size conflict); B X-Received-Body-CRC origin (which server software emits it, CRC input) + backbone Path/injection token catalogue from raw headers on Google Groups/archives; C pivot: Date header formatting/clock forensics per tool and server, 2-digit year etc.

## Wave 3 (07:48Z)
- A: NxG + Nyuu (+ngPost) exact output from source (codeload tarballs / jsdelivr)
- B: X-Received-Body-CRC origin + backbone Path/injection/Message-ID rewrite catalogue
- C: Date header formats per tool, server Date rules, session chaining

### W3B result (X-Received-Body-CRC / backbones)
- Origin NOT resolved. Second sample: Usenet.Farm article with User-Agent Nyuu/0.3.8, Message-Id <...-1587103108703@nyuu> (poster's own, not rewritten), Path not-for-mail, X-Received-Bytes 740517, X-Received-Body-CRC 3585757165, plus X-Ufhash header (Farm-specific!). Source: github spoved/nntp-client.cr src/nntp/connection/articles.cr (code-search snippet).
- TorBox news server reportedly emits X-Received-Body-CRC + lowercases "Message-Id" (debridmediamanager notes, unverified). EasyNews ReadMe lists it too.
- X-Received-Bytes 740517 on a Nyuu 716800-byte part ~= yEnc-encoded body size (~3.3% overhead) -> it's probably encoded body length, and CRC probably over encoded body. Hypothesis only.
- No backbone Path dumps found for Highwinds/Eweka/XS/UE/Giganews. Subagent: codeload + jsdelivr blocked (403) for it.
- Decisive test = live HEAD of the same article on 2 providers + crc32 over raw body variants.
- Posting Haiku15 W3: Postie full obfuscation Date = now - U(0,6h) per article (noise); NewsUP --obfuscate per-article random group subset + empty From; Injection-Info logging-data = nnrpd pid (one connection); Spotnet spots carry release NZB Message-ID = ground-truth labels.

### W3A result (source-verified, proxy.golang.org / registry.npmjs.org work for fetching source)
FINGERPRINT TABLE
- Nyuu 0.4.2: 716800 raw bytes, line=128; MsgID <([A-Z][a-z]){12}-\d{13}@nyuu> (Date.now ms; retries re-roll the 24 chars); headers Message-ID, Subject, From, Newsgroups, Date, Path(empty "Path: "), User-Agent Nyuu/x; Date toUTCString "... GMT"; =ybegin part total line size name / =ypart begin end; =yend size part pcrc32 (lowercase, padded) + crc32 last part only; escapes NUL LF CR =, TAB/SPACE at col 0 and line end, '.' at col 0; CRLF. Subject [NN/MM] - "name" yEnc (p/n) size.
- ngPost (mbruel): 716800; MsgID <32 lowercase hex uuid>@ngPost; headers From, Newsgroups, Subject, Message-ID only (no Date/UA/Path); From 13 rand chars@ngPost.com; Subject "name" (p/t) with NO yEnc token (obfuscated: Subject = MsgID); =yend size pcrc32 (lowercase, UNPADDED), no part=, no crc32.
- postie (javi11): default 768000 struct / 750000 fallback; via mnightingale/rapidyenc: =ypart begin end (no size), =yend size part pcrc32 lowercase padded, NO crc32 on last part; line 128; Go map -> random header order; Date RFC1123Z UTC (+0000); default full obfuscation, random IDs, X-Nxg off.
- nxg-upper (Tensai75): 768000; =ypart begin end size(!), =yend size part pcrc32 UPPERCASE padded + crc32 last; escapes 0x7F (non-standard!) + space at line start/end, not TAB; Date RFC1123 local zone abbrev; LF-only header lines; Newsgroups header never emitted (map key bug); random header order; Subject [F/T] <Comment> - "name" yEnc (p/n).
- NXG: header = base64(21 rand alnum with tail ":<data>:<par>") 28 chars. javi11 MsgID = sha256hex("<hdr>:<data|par2>:<n>") -> h[0:40]@h[40:61].h[61:64]; nxg-upper uses the Comment as key (incompatible). X-Nxg = hex(nonce12||AES-256-GCM("fileNo:totalFiles:filename:partType:totalSize", key=header padded to 32)) on every nxg-upper article. From derived from posterHash slices.
CONTRADICTIONS: Nyuu=ngPost=716800 (map is right, posting thread's 700K wrong-ish: 700 KiB). Go tools random header order -> order fingerprint useless for them, but randomness itself is a tell.
- Haiku10 yEnc W3: Highwinds server-assigned MsgIDs NzZUA.117908$rY1.16813@fx45.am4 then OzZUA.117909$... : counter +1 per article, host = injecting box (fxNN.<site>), sequence chains a session (source: sabnzbd tests/test_zip.nzb). They decode the format in W4 -> I avoid.
- Opus yEnc W3: V8 Math.random state recovery from floor(x*62) TESTED (32 outputs, exact prediction) -> one Nyuu-Obfuscation article links its session. X-Received-Body-CRC stamped at injection (Highwinds, Farm), not reader (claim). PowerPost 2004: no total=/crc32=, 640000-byte parts; yEnc32 escapes every '.'; old tools send no Message-ID.

### W3C result (Date forensics)
- Nyuu: toUTCString "Mon, 02 Nov 2026 07:48:54 GMT"; Date seconds == floor(MsgID ms/1000) (same genTime); mismatch => --date used; constant Date across run => --date now. Retries reuse stored header string (same Date+MsgID). Upload on N connections so arrival order != Date order.
- postie v0.0.24: Date.UTC().Format(RFC1123) => "... UTC" zone name; W3A saw v0.0.35 use RFC1123Z (+0000) => Date zone form is a postie VERSION marker. Full obfuscation: Date uniform [now-6h, now].
- RFC 5537: injector MUST add Date + Message-ID if absent (so ngPost Date = server time); SHOULD reject >24h future, past cutoff >=72h (7d conventional). INN: once Injection-Date exists Date isn't checked; nnrpd doesn't add Injection-Date if Date+MsgID present (2.6).
- Use: Injection-Date (server) preferred for timing; Date later than Injection-Date suspicious.

### Wave 3 merge
Big wins: source-verified fingerprint table for Nyuu / ngPost / postie / nxg-upper (yEnc field set, CRC case/padding, crc32-last, header order, Date form, escapes, segment size). Nyuu and ngPost both 716800. Date zone form fingerprints tool and version (GMT Nyuu, UTC postie old, +0000 postie new, none ngPost, local abbrev nxg-upper). X-Received-Body-CRC still unproven; Farm adds X-Ufhash too.
Fetch tip: proxy.golang.org and registry.npmjs.org work for source when GitHub is blocked.
Wave 4 plan: A remaining tools' source (ngPost Yenc.cpp escaping + Hydro74000 fork diffs, GoPostStuff via proxy.golang.org, NewsUP via GitHub mirror/metacpan, newsmangler, sabnzbd's own poster? , Usenet-specific Rust/Python posters on PyPI/crates: e.g. nyuu alternatives) to finish table. B real historical headers: archive.org Usenet mbox collections + X-Ufhash + backbone Path tokens (avoid Highwinds MsgID decode, Haiku10 doing that). C deterministic yEnc: do different encoders produce byte-identical yEnc for the same input at line=128? If yes, :bytes/X-Received-Bytes/Body-CRC become cross-tool twin keys from XOVER; quantify which escape-rule differences break it.
- Opus posting W3: INN server-assigned IDs encode time + nnrpd pid + counter; NewsUP fake GoPostStuff IDs misspell @gpoststuff (tell); postie NxG header charset never contains 'Z'.
- Payload Haiku W4: PAR2 exponent-0 recovery slice = XOR of all input slices, identical across renames/set IDs/tools given same bytes + slice size -> rename-proof twin key from one recovery article.

## Wave 4 (08:26Z; 08:15 trigger did not arrive, launched manually)
- A: remaining tool fingerprints (ngPost Yenc.cpp, GoPostStuff, NewsUP, newsmangler, packaged posters)
- B: historical raw headers from archive.org (backbone Path tokens, X-Ufhash, stamped headers)
- C: deterministic yEnc experiment (is encoded body / :bytes a twin key within and across tools)
- Haiku10 yEnc W4: NewsUP =yend size pcrc32 (no part=), headers From, Newsgroups, Subject, Message-ID. sanguinews: no Message-ID, X-Newsposter 'sanguinews v<ver> (ruby <ver>)', numeric zone Date, single-part =ybegin line size name/=yend size crc32. nntpPoster Date '+0000 (UTC)', uses server's POST 340 Message-ID; newsmangler too -> explains server-made fxNN IDs.

### W4A result (more fingerprints, source-verified)
- GoPostStuff v0.2.0: headers From, Newsgroups, Message-ID, X-Newsposter, Subject; no Date; MsgID <UnixNano$gps@gopoststuff>; Subject "[prefix ]subj [n/N] - "fname" yEnc (p/P)" (leading space when empty); =ypart begin end; =yend size part pcrc32 %08X UPPER padded; never crc32; size from config (sample 768000); madcowfred/yencode 2013 escapes NUL LF CR = , TAB/SPACE col0 + col127, '.' col0; wraps after append -> 129-char lines; no dot-stuffing on POST(!).
- ngPost Yenc.cpp: escapes TAB/SPACE at col 0 only (end-of-line check is dead code), '.' col0; 129-char lines possible; =yend size pcrc32 lowercase unpadded; body ends with "." line.
- nasauthunder-cli 0.2.3 (npm, new): MsgID <gcid[.index]@nasauthunder-v1.invalid>, Subject "nasauthunder <gcid> part n/total", Content-Type application/octet-stream, name=<gcid>.bin, 716800, minimal escaping only (NUL LF CR =), no 129-char lines.
- newsmangler/NewsUP source not reachable this time (peer thread has NewsUP: =yend size pcrc32, no part).
- 129-char lines (escape at col 128) = GoPostStuff/ngPost tell; Nyuu yencode escapes line-end chars properly.

### W4B result (archive.org blocked by egress proxy; snippets only)
- X-Ufhash (Usenet.Farm): base64 (URL-encoded %2B/%2F), ~64+ chars, e.g. prefix ZaQJEMf5JdvcWEOleKoXo4ljK0hv... ; a forum user decoded it and matched the first 4 bytes to an IP -> likely encrypted/encoded posting-IP or account token for abuse tracing. HYPOTHESIS: if deterministic per poster (not per article), X-Ufhash equality = same uploader connection/account: a "same uploader" key on Farm-injected posts. Needs 2+ samples from one session.
- Leo 2018 sample Message-Id <IgLnZpOzUcUfNzGyQaCiZfVw-1530199484291@PRiVATE> = exact Nyuu grammar ([A-Z][a-z]x12 - ms) with custom domain; ms 1530199484291 = 2018-06-28 15:24:44Z vs Date 15:25:10 (26s queue lag). So that Farm sample was a Nyuu post with --message-id domain change.
- eternal-september injection: "reader01.eternal-september.org!.POSTED!not-for-mail"; Giganews Path "border1.nntp.dca.giganews.com!nntp.giganews.com!" (2009). Highwinds fxNN.POSTED not confirmed by this agent (W2 saw "fx20.iad.POSTED" in an r/UsenetTalk quote).
- X-Trace legacy formats: "posting.google.com <epoch> <pid> <ip> (date)", panix same shape.

### W4C result (deterministic yEnc experiment; scripts copied to research/yenc-header-forensics/tools/)
- Same rules + same payload => byte-identical body (true twins from the same tool have identical body, :lines, encoded size).
- Within one tool, encoded size of different payloads at same raw size: sd ~140 (random) to ~195 (gzip) bytes; pair false-match ~1/400-1/750. :lines sd ~1.3 so adds nothing.
- Across rule sets, systematic offsets vs spec-minimal: strict break +2 B (+1-2 lines); Nyuu-style edge escaping +111..115 (sd 9-12); nxg 0x7F escaping +2900 (sd ~57). 129-char lines appear in overflow encoders (81/5687 lines).
- So encoded size is a within-tool filter, never a cross-tool equality key; XOVER :bytes includes headers (needs exact header subtraction).

### Main-session check (gh code search, 08:35Z)
- Full Usenet.Farm HEAD sample (spoved/nntp-client.cr): Nyuu/0.3.8 post, Message-Id <YwGnYrShOtJaBfSzZlTkKbBh-1587103108703@nyuu>, Date "Fri, 17 Apr 20 05:58:28 UTC", From random "3vq60fEnli <AEdwj0ie5p@kgiqA10.com>", 7 groups, X-Received-Bytes 740517, X-Received-Body-CRC 3585757165, X-Ufhash (228 chars URL-encoded base64 -> 171 bytes).
- FINDING: Nyuu writes Date via toUTCString ("Fri, 17 Apr 2020 05:58:28 GMT") and the MsgID ms = 05:58:28.703 matches exactly, yet the served Date is "17 Apr 20 ... UTC". Both Farm samples (2018, 2020) show 2-digit year + "UTC". => Usenet.Farm REWRITES the Date header format (keeps the time). Also header casing "Message-Id". That's a backbone fingerprint readable from XOVER Date alone. (TorBox note of casing change suggests TorBox is Farm-fed.)
- X-Ufhash: variable length (171 B, 192 B for a Farm spam sample with Message-Id @news.usenet.farm) => encrypted variable plaintext (abuse-trace blob), not a fixed-size hash. Equality across a session untested.
- X-Received-Bytes 740517 vs predicted encoded body ~739480 + yEnc lines 171 + headers ~442 = ~740090 -> X-Received-Bytes counts the whole received article incl. headers (some later stripped), not body only. So it's not a pure payload key; X-Received-Body-CRC input still unknown.
- Farm-injected spam sample shows server-assigned MsgID <32 alnum@news.usenet.farm> when the client gives none.

### Wave 4 merge
Wins: tool table nearly complete; Farm Date rewrite = backbone fingerprint; encoded size is within-tool only; X-Received-Bytes = whole article.
Messaging: limit to Haiku10 yEnc, Opus yEnc, catalog (40 msg/h cap memory).
Wave 5 plan (08:55Z): A per-backbone header rewriting (Date format, casing, added/stripped headers) using gh code search snippets of real HEAD dumps (Highwinds/Omicron, UE, Abavia, Giganews, Eweka, Newshosting, Easynews, TorBox); B XOVER :bytes model: exact header byte formula per tool + what :bytes counts (INN/Diablo/Highwinds), to make a within-tool XOVER twin filter; C public NZB search engines (binsearch, nzbindex, nzbking, newzleech, yabsearch) grouping/dedupe prior art.
- Opus yEnc W4: per-part :bytes/:lines VECTOR over a whole file = twin key (~120 bits per 70-part file; each part ~7-8 bits per W4C sd). Counter-bearing IDs self-check (PowerPost partNofM, JBinUp _NoM, JBinDown N-M); NewsUP fakes fail; NewsUP ID leaks drand48 state; ngPost Windows build is MinGW. -> W5B should focus on making header subtraction exact so the per-part vector works across groups/servers.

## Wave 5 (08:57Z)
- A: backbone header rewriting via gh code search snippets
- B: XOVER :bytes/:lines model + header subtraction formula per tool
- C: public NZB search engines / newsreaders grouping and dedupe prior art

### W5A result (backbone header fingerprints via gh code search)
- Newshosting (Highwinds/Omicron) 2014-15 text posts ALSO carry X-Received-Bytes + X-Received-Body-CRC + "Bytes:" + NNTP-Posting-Date "Thu, 13 Mar 2014 20:41:40 UTC" (4-digit year, UTC) + X-Complaints-To abuse@newshosting.com + Organization "Newshosting.com - Highest quality at a great price! www.newshosting.com". So X-Received-* is reader-software (likely Highwinds-family, also used by Farm), not Farm-only. (GerHobbelt/FontOrg-and-FontRenamer txt backups)
- Highwinds 2009 Path: ...!news.highwinds-media.com!feed-me.highwinds-media.com!post01.iad.highwinds-media.com!newsfe22.iad.POSTED!7564ea0f!not-for-mail -> token "<host>.POSTED!<8hex>!not-for-mail" (the hex is likely an account/poster hash = same-uploader key if stable!). (larsbrinkhoff/its-archives usenet/2009-*)
- Giganews 2007: Path "news.giganews.com.POSTED!not-for-mail", NNTP-Posting-Date "-0500", X-Authentication-Warning.
- Usenet.Farm server MsgID when client sends none: <32 alnum@news.usenet.farm>.
- Eweka/UE/Abavia/Easynews/Omicron/TorBox: no real dumps found.

### W5B result (XOVER :bytes model; tools/xover_bytes_model.py scratch)
- INN innd counts CRLF as 1 byte and excludes the header/body blank line and trailing ".\r\n"; tradindexed tdx-util stats the stored file (everything). So :bytes semantics differ per storage backend even on one server (Russ Allbery, inn-workers 2002-09). Lines excludes headers.
- Recipe: body_i = :bytes_i - visible header length (Subject, From, Message-ID, Date from XOVER; Newsgroups from Xref) - per-server constant; then difference vector d_i = body_i - body_1 cancels unseen per-post headers (Path/UA/Organization/X-*). Match files on d within +-3 B/part; :lines exact as coarse filter (~2.5 bits/part vs ~9.5 bits/part upper bound for bytes).
- RFC 3977 s8.1 text not retrieved (unverified).
- Haiku10 yEnc W5: Highwinds server IDs T1.N$K.R@fxNN.site (658 real IDs): N front-end article counter across post (gaps), K constant per session = session key, T1 clock-like base64, R random. Hydro74000 ngPost pcrc32 %08x (mbruel unpadded); both no part= on =yend. newspost: no Message-ID/Date. Nyuu adopts server-returned ID (lib/nntp.js:866-875).
- Opus yEnc W5: Nyuu NZB <segment bytes> = yEnc body + =y lines + 3 exactly (104 real NZBs) -> per-segment escape counts exact from the NZB (no header noise!) = twin vector without XOVER header subtraction; NZB-writer fingerprint. RNG session linking reimplemented: Nyuu-Obfuscation 60/60 from one article, NewsUP, ngPost.

### W5C result (prior art grouping/dedupe)
- Newsbin: RIPEMD over first 24K/26K, false positives on shared intros. SABnzbd: name-based dupes; warns "Duplicate part 1, but different ID-s" (crosspost copies). NNTmux: collection_regexes, collectionKey sha1(name+totalFiles), Xref parsed. GoNZB (Go, 2026, github datallboy/GoNZB docs/wiki/indexer/binary-grouping-evidence.md): identity = normalized filename/canonical subject token + size + file idx/total + part/total; NOT From/MsgID/article number; same name+part with different MsgIDs = duplicate copies; yEnc BODY only when HEAD incomplete; BODY probe budgets 25k-100k/h, stop cohort after 16-32 empty probes. Weaver (Rust, scryer-media) is_obfuscated = 30+ contiguous hex/dot chars, name confidence quoted 1.0 / PRiVATE 0.8 / heuristic 0.5. nzbfast "Pesto" obfuscates Subject, yEnc name, From, Date but posts a real-name PAR2 AFTER the payload (fingerprint + name recovery).

### Wave 5 merge
- X-Received-Bytes/Body-CRC are also on Newshosting (Highwinds) 2014 posts -> a Highwinds-family reader header, not Farm-only. Highwinds Path token "<fe>.POSTED!<8hex>!not-for-mail" (hex = account/connection hash? potential same-uploader key). Highwinds server IDs T1.N$K.R@fxNN.site with K per session (peer).
- XOVER twin recipe: difference vector of header-corrected :bytes per part; better: Nyuu NZB segment bytes = body + y-lines + 3 exactly (peer) -> no header noise.
Wave 6 plan (~09:30Z): A Highwinds/Omicron server software forensics (POSTED!<hex>! token meaning, X-Received-Body-CRC definition, Typhoon/Cyclone/Twister docs, X-Trace-like fields); B Go/Rust indexer prior-art source (GoNZB via proxy.golang.org, Weaver, nzbfast/nzbkit via crates or code search) for grouping code and obfuscation detectors; C obfuscated-name grammars per tool (subject, yEnc name, filename, password length/charset) as a tool classifier.
- Haiku10 W5 update: Highwinds T1 decoded = little-endian base-62 (0-9a-zA-Z), unix time = value + 876645500 -> second-exact time + order (N) + session (K) from XOVER Message-ID. They leave the Path 8-hex to me. Test: does Path hex == K per post?

## Wave 6 (09:21Z)
- A: Highwinds Path POSTED!<hex> meaning + X-Received-Body-CRC input test on small text posts
- B: GoNZB / Weaver / nzbfast source (grouping + obfuscation rules)
- C: obfuscated random-string generator catalogue per tool (classifier)

### W6C result (obfuscation-string classifier, source-read)
- Nyuu default MsgID ^(?:[A-Z][a-z]){12}-\d{13}@nyuu$ ; NewsUP's Nyuu fake ^.{12}-\d{16}@nyuu$ (16 digits = tell).
- Nyuu rand template / postie Full / Go-Post-Tools: Subject ^[A-Za-z0-9]{32}$, MsgID ^[A-Za-z0-9]{32}@[A-Za-z0-9]{8}\.[A-Za-z0-9]{3}$, From ^[A-Za-z0-9]{14} <...{14}@{5}.{3}>$ (Go-Post-Tools no brackets). Collision: separate by Date form (postie RFC1123Z/"UTC", Nyuu GMT), header order (postie random), RNG.
- postie Partial: Subject ^[0-9a-f]{32}$ (md5 subject), yEnc name md5(filename+part) 32 hex.
- postie nxg: MsgID ^[0-9a-f]{40}@[0-9a-f]{21}\.[0-9a-f]{3}$, Subject sha256 64 hex, From ^[0-9a-f]{5} <[0-9a-f]{15}@[0-9a-f]{15}\.[0-9a-f]{3}>$.
- ngPost: From ^[A-Za-z0-9]{13}@ngPost\.com$ (bare), MsgID ^[0-9a-f]{32}@ngPost$, obfuscated Subject == MsgID local; RAR name 17 / password 13 alnum (std::rand).
- NewsUP: Subject ^[a-z]{1,32}$, From ^[A-Za-z0-9]{8} <.{0,17}@...>, fakes JBinUp ^[A-Za-z0-9]{32}@JBinUp\.local$, PowerPost ^part\d{1,2}of\d{1,2}\.[A-Za-z0-9_-]{22}@(camelsystem-powerpost|powerpost2000AA)\.local$, GoPostStuff ^\d+\.\d+\$gps@gpoststuff$ (misspelt + dot, real is UnixNano$gps@gopoststuff).
- franzopl/pesto (Rust): MsgID ^<[0-9a-f]{32}@[a-z]{8,15}\.(com|net|org)>$, From ^[a-z]{5,12} <[A-Za-z0-9]{10,20}@[A-Za-z0-9]{5,10}\.[a-z]{2,5}>$. nzbfast pesto.rs describes <16hex>.<4-5hex>.<16hex>@domain (different version).
- weaver classify.rs: tool by MsgID domain substring at 90% share (misses Nyuu-Obf/postie/pesto).

### W6A result *** X-Received-Body-CRC SOLVED ***
- X-Received-Body-CRC = zlib.crc32(body with CRLF line endings + ".\r\n"), body = everything after the header/body blank line, headers excluded. 12 real articles match (GerHobbelt/FontOrg-and-FontRenamer Newshosting posts + Usenet.Farm doc example). Main session re-verified upd.txt: 961135873 == crc32(body_crlf + b".\r\n"); without the dot line it is 2262295484 (miss). Scripts: tools/xrbc/run6.py etc. Dot-stuffing untested (stuffed form likely, as it's the wire form).
- Consequence: for a yEnc part, body = =ybegin/=ypart lines + encoded data + =yend line. Same tool + same yEnc name= + same part boundaries + same payload => identical X-Received-Body-CRC => a byte-level TWIN KEY from HEAD/HDR without BODY. Crosspost copies obviously share it. Breaks when the yEnc name is randomized per post or the encoder differs. Also lets Atlas verify article integrity after BODY.
- X-Received-Bytes ~ wire size (off by 1 on one sample), not exact.
- Highwinds Path hex after POSTED! = per-feed/posting-cluster constant (same poster 2009 always 7564ea0f; gnilink hosts share a few values); not a hash of hostnames. Weak uploader signal at best.
- Highwinds software: Typhoon NewsServer, Cyclone NewsRouter, Tornado (reader/cache), Hurricane (numbering), Stormcellar.
- GoNZB doc: X-Received-Bytes is transfer-encoded evidence. TorBox reader returns X-Received-Body-Crc/Message-Id normalized casing, Path not-for-mail (Omicron-type reader).
NEXT: live check whether Eweka (Omicron) serves X-Received-Body-CRC via HDR/XHDR/XOVER-extra (LIST OVERVIEW.FMT) -> bulk fetch cost.

### W6B result (Go/Rust prior art source)
- GoNZB v0.10.4: cross-group regroup key (provider, release_family, lower(file_name), file_index, expected_file_count, total_parts), needs >1 group, span <=6h; additive matcher (quoted name 0.42-0.5, yEnc marker +0.12, structured yEnc name +0.30, poster +0.05, window +0.03, article bucket +0.02; 0.85/0.55 thresholds); yEnc body-prefix recovery cached by Message-ID.
- Weaver deobfuscate.rs rule list (hex/dot >=30, 16+ hex, mixed alnum >=14, uniform-case alnum >=24 ...); analysis/mod.rs "repeats" = identical sorted segment Message-ID set = exact twin.
- nzbfast/nzbkit: pesto MsgID <16hex clock>.<4-5hex counter>.<16hex rand>@domain, counter = session key; links payload to PAR2 via counter adjacency + length ratio <=1.035 + 16k-MD5 gate (64.6% linkable, 8/330 mislinks without gate). ingest contradicts(): same filename + different total, or different MsgID at same part => second posting. NameEvidence ladder BodyProbe > MsgidSet quorum > Par2SetId > Md5Manifest > SparseBlake3 > Crc32Len > Hash16kLen > Adjacency. yEnc name majority vote, incumbent on tie. sessionfold: >=4 files, one poster+group, span <=3600s, size within 2%.
- altmount isProbablyObfuscated (SABnzbd port), default OBFUSCATED; selectBestFilename PAR2 +3 > subject +2 > yEnc +1.
- ngPost tryResend assigns NEW UUID -> one part under two IDs (twin-within-post).
- nxg-loader rebuilds all IDs from nxglnk:// links without an NZB.

### Wave 6 merge
Headline: X-Received-Body-CRC formula solved (twin key from HEAD). Prior art gives concrete rules (contradicts(), NameEvidence ladder, obfuscation detectors).
Wave 7 plan (~09:50Z): A bulk header access: CAPABILITIES / LIST OVERVIEW.FMT / LIST HEADERS / HDR/XHDR/XPAT support on Omicron/Highwinds, Abavia, UE, Farm (can X-Received-Body-CRC be fetched in bulk?); B NZB writer fingerprints + exact semantics of <segment bytes> per writer (Nyuu body+ylines+3, ngPost, postie, nxg-upper, SABnzbd/NZBGet re-writers, newznab/NNTmux/nZEDb from XOVER, nzbhydra) and <file poster/date> semantics; C nxglnk:// links in the wild + nxg-loader (where shared, format, how an indexer could precompute and STAT).
- Haiku10 W6: Highwinds K constant per whole post, differs between posts -> post/session key, not account. T1 alphabet 64 symbols incl _ and %. ubuntu-mate NZB on GitHub has <random>@UFK Message-IDs (Usenet.Farm? cf X-Ufhash).
- Opus yEnc W6: stock Nyuu default IDs RNG-solvable on 2025-26 posts (older V8 fails); escape-count key matched real repost 15/15; NZB writer stamps (Nyuu utf-8 vs UTF-8 dates version; Binsearch/NZBIndex/newznab/nZEDb/NZBgeek comment stamps). -> narrow my W7B to <segment bytes> exact semantics per writer only.

## Wave 7 (09:52Z)
- Launch was interrupted; A (provider bulk header access) may be running; B/C not launched. Will merge whatever arrives and move on.

### W7A result (bulk access to X-Received-Body-CRC)
- Highwinds/Eweka per Spottarr ADR-0006: CAPABILITIES list XZVER, XZHDR, XFEATURE-COMPRESS GZIP TERMINATOR, no COMPRESS; eweka sends zlib (78 01) despite saying GZIP; XZVER ~3.9x on 1000 rows. Captured CAPABILITIES (torbox/elfhosted notes): VERSION 2, READER, OVER, HDR, XZVER, XZHDR, LIST ... HEADERS ... So HDR is advertised.
- No evidence yet that XHDR/HDR X-Received-Body-CRC is accepted (RFC 3977 allows 503 for non-overview headers).
- Cost (1M articles, 50 ms RTT): pipelined HEAD depth 50 ~1000 s + ~1 GB; XHDR 1000/req ~20 MB (if accepted). Recommendation: probe XHDR X-Received-Body-CRC on a small range per provider once; else pipelined HEAD.

### W7B result (NZB segment bytes semantics; this agent launched despite the interruption)
- Nyuu: NZB bytes = post.postLen = body from "=ybegin" through "=yend ...\r\n.\r\n" exactly, no headers. Body: "=ybegin part=p total=T line=128 size=S name=F\r\n=ypart begin=B end=E\r\n" + data (CRLF before each new line, none after last) + "\r\n=yend size=L part=p pcrc32=h[ crc32=h]\r\n.\r\n". No dot-stuffing (leading '.' is escaped). Deterministic; Nyuu re-checks length on repost.
- => Nyuu NZB segment bytes == exactly the byte length of the input that X-Received-Body-CRC hashes (body CRLF + ".\r\n"). Same object: NZB gives its length, HEAD gives its CRC32. Twin key pair (len, crc) for Nyuu posts.
- pesto (franzopl, Rust): NZB bytes = header block + yEnc body, without ".\r\n" -> not comparable to Nyuu; needs writer detection. pesto single-part "=ybegin line size name" (no part/total).
- Payload Opus W7: NZB-only index for Nyuu-style NZBs: k=8 middle-part escape counts -> 64-bit key, ~0 false/day at 1e8 files, recall 1.0 for reposts + mtime re-RARs; ngPost NZBs useless (bytes = 716800 raw).

## Wave 8 plan (launched 10:15Z, cap 10:30Z)
- W8A: dot-stuffing in X-Received-Body-CRC input; public evidence of XHDR/HDR/LIST HEADERS on Highwinds/Omicron/Eweka/Farm/UsenetExpress; other server-added headers.
- W8B: NZB <segment bytes=> semantics across posting tools and indexers (source-verified table).
- W8C: prior art for storing/scoring links (newznab-tmux/nZEDb collection hashes, NZBHydra/SAB/nzbget dupe keys) + 5 link-table design ideas.

### W8C result (indexer dedupe prior art, 10:15-10:18Z)
- NNTmux collectionhash = sha1(regex subject parts + fromname + groupID + "N/M"); nZEDb binaryhash = md5(subject match + From + group id). Keyed per group: NO cross-group merge in newznab lineage. nzb_guid = md5(first segment Message-ID), BINARY(16).
- SABnzbd md5sum = md5 over all segment Message-IDs in NZB order (exact-NZB dupe); duplicate_key = title/season/episode. No size tolerance.
- nzbget DupeCoordinator: dupekey or name; sameContent = FullContentHash or FilteredContentHash equal; dupescore replaces lower.
- NZBHydra2 DuplicateDetector: title lowercased, strip [ .-_]; differ if both groups known and differ, or both posters known and differ; age hours and size % of mean thresholds (doubled if only one of group/poster matches).
- Takeaway for Atlas: prior art never links across groups or across reposts (new Message-IDs). Our body-CRC / size-vector twin keys are new ground. Borrow: nzb_guid style stable id, NZBHydra pairwise veto rules for alt candidates, links(a<b, kind, score, evidence JSON).

### W8A result (dot-stuffing / provider headers, 10:15-10:19Z)
- Dot-stuffing: no source found; still open. Test: an article with a body line starting "." (yEnc encoders that do not escape column-0 dots), crc both stuffed and unstuffed.
- Public saved articles (GerHobbelt/FontOrg-and-FontRenamer, 2014-15, Newshosting/highwinds) carry X-Received-Bytes + X-Received-Body-CRC, so the header is >10 years old on Highwinds. One value is 901070 (6 digits, plausible as a small uint32, printed without padding: decimal, unpadded).
- spoved/nntp-client.cr fixtures: Usenet.Farm articles with the same pair plus X-Ufhash, Date with 2-digit year. Matches W4-W6.
- NEW caveat: TorBox news server (debridmediamanager/notes, zurg/providers/torbox-news-server.md) normalizes header casing (X-Received-Body-Crc, Message-Id) and forces Path: not-for-mail. Atlas must parse header names case-insensitively and not treat casing/Path as poster fingerprints when the reader is a reseller/proxy.
- Newshosting "Bytes:" header does not equal X-Received-Bytes (1201 vs 1251, 1343 vs 1320).
- No public XHDR/HDR evidence for X-Received-Body-CRC. A live probe is still the only way to settle it.

### W8B result (NZB segment bytes= semantics, 10:15-10:20Z)
- Wire-size family: Nyuu (body =ybegin..\r\n.\r\n), pesto (headers+body).
- Raw-size family: postie (segmentSize, last = remainder), ngPost (_fileBytes), NZBGet NzbGenerator (min(segsize, remaining)), newsmangler (article_size config).
- XOVER family: newznab/NNTmux, nZEDb, nzbsearcher re-emit XOVER :bytes (server-defined, headers usually included).
- Readers: SABnzbd drops segments outside 0<n<2^23 ("strange size"), sums bytes; NZBGet sums for file size.
- Rule for Atlas: classify an NZB's bytes= family before comparing. Raw-family bytes are constant per part (useless for twin keys except the last-part remainder = file size mod part size). Only Nyuu-family bytes compare to X-Received-Body-CRC length; XOVER-family needs header subtraction (W5 recipe).

### Peer input 10:23Z (payload Haiku15 W8)
- par2cmdline and ParPar default to recovery exponent 0, so the exp-0 slice key needs only a matching slice size. ngPost makes PAR2 only with --par2_pct.
- crc32fast has combine: whole-file CRC can be derived from the yEnc pcrc32 values plus part sizes, so a missing =yend crc32 can be filled in. W9 idea.

### Peer input 10:29-10:30Z
- Opus payload W8: encrypted RAR5 -hp / 7z -mhe reposts keep identical volume sizes; (V, n, S_last) ~27.5 bits as a blocking key, plus one exact file size gives <1e-8 false pairs. Disc posts: AACS Unit_Key_RO.inf SHA-1, UDF Volume Set ID as per-master keys. Use xxhash for persisted keys, never ahash.
- Opus yEnc W7: Nyuu's published ID comes from randomizeMessageID() at send time; pre-Node-11 posts break the RNG solver. [A-Za-z]{32}@UFK is server-assigned. Deterministic linkable IDs: geri (HMAC incl. crc32), keimpema 'part{n}of{N}.{guid}@unknown.com', Upstream '@riverup' microsecond time.

## Wave 9 plan (launched 10:30Z, cap 10:45Z)
- W9A: pcrc32 -> PAR2 IFSC slice CRC32 via crc32 combine; which tools align article size to slice size; existing software that does this.
- W9B: real yEnc sample corpora (test fixtures, saved articles) to verify the fingerprint table, 15+ samples.
- W9C: Subject templates + name= behaviour for tools not yet verified (JBinUp, PowerPost, yEnc32, newzNZB, nntpPoster, NewsUP, sanguinews, newsmangler, UE, BinTube...).

### W9B result (yEnc sample corpora, 10:30-10:34Z; mostly encoder templates, not wire captures)
New or confirmed encoder tells (source templates):
- ngPost: pcrc32 via std::hex, NOT zero-padded (variable length, 1-8 hex). Strong tell when value < 0x10000000 (~6% of parts).
- GoPostStuff, Blat, rustnzb: uppercase hex (%08X). rustnzb also omits part= on multipart =yend.
- nzbget YEncoder (nserv test server): part=0 on single-part =yend.
- xHarbour: trailing space before CRLF on every =yend. yEnc 1.3 draft example also has a trailing space.
- newspost: single-part uses crc32= (no pcrc32); multi =yend written without its own CRLF.
- Many test/indexer tools (decypharr nntpd, sabctools invalid fixtures) omit total=.
- NZBreX nntpseed, node-yencode README, Nyuu, Sportarr: crc32= on the last part only, same line as pcrc32.
- newsreap single-part =yend has no CRC at all.
- No public wire capture with User-Agent + yEnc body found. Real verification needs our own article samples.

### W9A result (pcrc32 -> PAR2 slice CRC, 10:30-10:35Z)
- crc(A||B) = shift(crc(A), 8|B|) XOR crc(B) (zlib crc32_combine_op / x2nmodp; pesto yenc/mod.rs; sabctools crc32_combine). Conditioning cancels. Suffix removal and zero-pad of a short last slice also work (SABnzbd crc32_zero_unpad; pesto penne pad_crc32_to_slice_boundary).
- Non-aligned slices cannot be derived from pcrc32 alone (need the partial bytes).
- Alignment defaults: postie sets PAR2 block = article size (or multiple) by default; juicenet-cli -s700k --slice-size-multiple=700K; ParPar alignment opt-in; Nyuu/ngPost do not do PAR2 slicing; par2cmdline -s is block count, unaligned.
- Prior art: NZBGet ParQuick compares whole-file CRC (combined segment CRCs) with combined PAR2 IFSC CRCs; pesto penne folds IFSC CRCs. Nobody matches per-article pcrc32 to IFSC slices.
MERGE / NEW SIGNAL:
1. Split-independent file key: fold all pcrc32 with crc32_combine using =yend size= -> whole-file CRC32. Identical across reposts with different article sizes or tools (Nyuu 700K vs ngPost 716800), so it links twins/alts-in-same-container across tools. Cross-check against =yend crc32= when present.
2. PAR2-to-post binding without payload: when slice = m*article (postie default, juicenet), slice CRC32 = fold of m pcrc32; matching IFSC slice CRCs proves a PAR2 set belongs to a post (or to an orphaned split post) even when names are obfuscated. Whole-file level always works (NZBGet ParQuick style): fold IFSC CRCs (unpad last) == folded pcrc32.
Cost: pcrc32 lives on =yend (body tail). Atlas already fetches bodies for some parts; the last-part crc32= gives the file CRC from one body.

### W9C result (subject templates, 10:30-10:37Z)
- NewsUP: `[i/N] - "file" (p/P)` with NO "yEnc" token, unpadded; obfuscate randomizes subject (1-33 lowercase chars) but keeps the real name= (tell: random subject + real name=).
- sanguinews 0.80.1: `[sanguinews] - "file" yEnc (p/P)` default prefix; folder mode unquoted `dir [i/n] - name yEnc (1/n)`.
- ypost: `[prefix] "file" yEnc (p/P) N bytes [suffix]` ("bytes" suffix).
- newsmangler: `text [i/N] - "file" yEnc (pp/PP)`: part index zero-padded to total width, file index unpadded.
- newspost: `text - "file" yEnc (01/12)` (doc inconsistent).
- pesto: `"name" yEnc (p/P)`, no file index; obfuscation modes none/light/full-shared/full (full = random subject and name= per file).
- Easy Post: `Subject [a/b] - "file" yEnc (x/y)`. upapasta wraps Nyuu.
- No source for JBinUp, PowerPost, yEnc32, newzNZB, UE, BinTube.

### Peer input 10:42Z (payload Haiku15 W9)
- TESTED: folded article pcrc32 == whole-file CRC32 from PAR2 IFSC slice CRCs (last slice un-padded). Script: research/payload-fingerprints/haiku15/tools/slice_vs_article.py. Confirms W9A signal 1+2.

## Wave 10 plan (launched 10:43Z, cap 10:58Z)
- W10A: overview :lines semantics and Lines as an XOVER-level twin key.
- W10B: which yEnc fields downloaders trust vs which obfuscators change (trust ranking for scoring).
- W10C: Xref / crosspost vs multipost forensics, server-name tokens, indexer handling.

### W10B result (field trust, ~10:43-10:50Z)
- Content-bound (must be correct or downloads fail): yend pcrc32/crc32, yend size (NZBGet dsInvalidSize, pesto raises), ypart begin/end, single-part ybegin size, ybegin part= (selects mode/CRC key).
- Free / ignored by SAB+NZBGet: ybegin line=, total=, yend part= (nzbkit alone rejects part mismatch), ybegin name= (naming only; per-part name differences unchecked).
- Missing CRC: NZBGet silently passes; SAB treats as BadData. Overlapping begin/end not detected anywhere.
- Obfuscators: Nyuu randomizes only via --subject ${rand(N)}, --yenc-name, --date; ngPost --obfuscate renames files (random name=) and blanks From; sizes, offsets and CRCs stay real in both.
- Scoring rule: content-bound fields (pcrc32, sizes, begin/end) are identity evidence; free fields (name, line, total presence, yend part presence) are tool/uploader fingerprints only, never identity.

### W10A result (overview :lines, 10:43-10:48Z)
- RFC 3977 erratum 1932: :bytes and :lines are server-computed. INN innd/art.c: :lines = innd's own body-line count (client Lines: header ignored for overview; linecountfuzz default 0). INN :bytes counts the whole article incl. headers minus terminator/dot-stuffing.
- No posting tool checked sends Lines: (Nyuu confirmed none).
- Model: body lines L ~= ceil((B+E)/128) + 3 (multipart). std(E) ~105 for a 700 KB part, so per-part L carries ~1.5-2 bits.
- MERGE / NEW SIGNAL: the per-part :lines vector is a header-independent XOVER twin key. Unlike :bytes it does not move with Subject/From/Message-ID length, so it survives a repost by a different poster or with new subjects, provided the same line length (128) and the same escape set. Over a 70-part file ~100 bits. Cheapest twin signal there is (overview row, no HEAD). Caveat: backends other than INN unverified; check for blank/0 :lines per provider.

### Peer 10:46Z: payload-fingerprints final report https://claude.ai/artifact/4encPYR1gKbgmAU9vQQ76g (research/payload-fingerprints/haiku15/final-report.md); lists X-Received-Body-CRC as T0 per-article twin candidate.

### W10C result (Xref crossposts, 10:43-10:49Z)
- RFC 5536 3.2.14: Xref server-name says which server made it; non-xrefslave servers rewrite Xref on ingest (INN). INN requires Xref:full in overview. Tokens seen: highwinds, news-big.astraweb.com, news.easynews.com, news.usenetserver.com, article.news.xs4all.nl.
- NNTmux collectionKey = sha1(name + totalFiles) (no group), Xref groups to collection_groups: crossposts AND same-name multiposts merge. nZEDb: per group unless poster is in manual multigroup_posters (MGR). GoNZB: same Message-ID in several groups = one article; same name+part with different IDs = duplicate copies; xref weight 0.05.
- Tools: Nyuu -g a,b = one crosspost; postie group_policy all = crosspost, default each_file = one random group per file (multipost pattern); ngPost GROUP_POLICY all/one per post/one per file.
- Xref article numbers are per server and gappy: not timing evidence. Xref server token = which reader Atlas used, not where the post was injected.
- Rule: crosspost = same Message-ID (dedupe by ID across groups, store Xref groups); multipost/twin = different IDs with the same content keys (X-Received-Body-CRC, folded pcrc32, :lines vector).

## Wave 10 merge / end of waves (10:55Z)
Final report: Claude Doc https://claude.ai/artifact/ExnooEu7pBdBqoNvXeAbQT

### Peer 10:52Z (payload Haiku W9): par2cmdline default slice = ceil(total/2000) rounded to x4 over all files (extra file breaks exp-0 key). srrDB /api/search/archive-crc:<hex> searches inner-file CRC32 (robots-disallowed; sparing use).

### Peer 10:52Z (Opus yEnc W8): stock Nyuu Node 12+ IDs link within one NZB (2 IDs suffice), never across NZBs. Consolidated 33-variant table + 9-step classifier: research/yenc-header-forensics/opus/w8c-fingerprint-table.md. Notes for fingerprints.md: JBinUp/JBinDown regexes look swapped; Nyuu redraws 24 letters on every send incl. retries.

### Peer 11:31Z (Opus yEnc W9): size-vector twin key confirmed on a 2nd real repost event (7-vol RAR x4 in 43 min, renames; damaged copy 14/15 -> allow n-1; PAR2 regenerated never matches). Entropy gate segment sd 40-600 removes false groups (55/75 -> 0). Blocking key: segment count + first 2 sizes. Rust spec, 6-phase rollout: research/yenc-header-forensics/opus/w9b-atlas-rust-spec.md.

### Peer 11:31-11:32Z: Opus payload final map https://claude.ai/artifact/Kokmceq4dVK46C4oDHaKgD (no key merges alone; twin needs size+CRC32+16k/full MD5). Haiku payload x3 final https://claude.ai/artifact/4H6dyXh6RETqWKAK8v7phC.

### Peer 12:10Z: Opus yEnc final: research/yenc-header-forensics/opus/FINAL-yenc-header-forensics.md. Size-vector key: exact only <6 segments, n-1 from ~6, never n-2; second blocking key on segments 3-4; candidate generator only, confirm with size+CRC32+MD5.
