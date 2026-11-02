# Posting-behavior research log

Angle: behavioral and temporal correlation (cadence, article-number gaps, multi-group spray, poster rotation, propagation, clustering).
Started 2026-11-02 06:45Z. 10 waves × 3 Haiku agents (medium, 15 min cap), ~30 min per wave.

## Wave 1 (06:45Z)
- A: uploader tools (ngPost, Nyuu, GoPostStuff, newsmangler, JBinUp, powerpost, camelsystem): Message-ID, From, Date and part-ordering fingerprints
- B: multi-group spray, crosspost vs split-across-groups, Xref, per-file poster rotation, how indexers (newznab, nZEDb, spotweb, binsearch, NZBKing) assemble collections
- C: academic: Usenet measurement, propagation, temporal clustering of spam/campaigns, Sybil/coordination detection

> Sharing rule (06:46Z): after each wave, message the other 9 research threads a short summary and append best leads to docs/research/linking-findings.md under "Haiku 15-min research: posting behavior".

### W1C result (academic): returned 06:5xZ
No academic work on Usenet binary linking found. Transferable: CopyCatch lockstep cores (WWW 2013), "Setting the clock" temporal window sweep (arXiv 2609.21959), multiplex temporal coordination graphs (arXiv 2512.19677), spam campaign clustering (Haider ICML 2009 Bayesian; incremental/streaming clustering thesis, Utrecht), BitTorrent publisher linking (Cuevas arXiv 1007.2327, TorrentGuard 1105.3671). Unread lead: USPTO delivery-performance patents 7490148/8621076/9124473 (article propagation delay). Not yet checked: Kleinberg bursts, SynchroTrap.

### W1A result (uploader tools)
Verified from Nyuu help-full.txt and ngPost README only.
- Nyuu Message-ID `${rand(24)}-{timestamp}@nyuu`; timestamp = per-post encode time (--date), so one file's articles give a tight monotone timestamp run even when subject/From/name are obfuscated. Message-IDs re-randomized per submission (no --keep-message-id), so Message-ID is not a twin key.
- Nyuu default From leaks local username@host unless -f is set. Defaults: 700K article, 128-byte lines, 3 connections, yEnc name = real filename even when subject is custom.
- ngPost: -x gives a UUID subject per article (retries get a new UUID); Message-ID suffix default "@ngPost" (-z); --gen_from gives a random From per post; article 716800; threads = core count, 2 pre-encoded per connection; auto_compress name length 17 and pass length 13 by default.
- Open: ngPost source for exact Message-ID format; PowerPost, GoPostStuff, JBinUp, NewsUP, Camelsystem unchecked.

### W1B result (multi-group spray)
- No public collectionhash formula, Xref or crosspost heuristic found (search only, no source read).
- PAR2 per-file MD5 as a repost and slice join key (r/newznab thread); hashes in spare header fields need per-poster trust.
- yEnc segment fingerprint (size, part, begin/end, pcrc32) for byte-identical segments. yencoding crate notes: name= takes rest of line, all fields optional.
- kkm/nzb-indexer: partrepair retries missing article numbers 5x; holds incomplete releases ~4h for late parts.
- Backbone divergence (DMCA) means missing Message-ID on one server is removal, not a separate post.

### Wave 1 merge
Agents used ~1 min of 15, so wave 2 prompts demand source reading and a minimum number of fetches.
Strongest leads: per-article encode timestamps (Nyuu Message-ID), tool suffix in Message-ID (@nyuu, @ngPost), yEnc segment fingerprint, partrepair + hold window for late parts, CopyCatch lockstep cores.
Dead ends: academic Usenet-binary linking papers (none); forum search for collectionhash.
Plan wave 2: lean in on source code (uploaders, nZEDb/newznab-tmux collection code), pivot C to server-side injection headers (Path, Injection-Info, X-Received, NNTP-Posting-Host, Xref) as session links.

### Peer inputs after wave 1
- yEnc forensics threads: encoder fingerprint from =ybegin/=yend field order, hex case, total= presence, crc32 on every part; newsmangler Message-ID `<unixtime.5dp.part@host>` (no randomness); subject zero-pad width; Injection-Info posting-account; Xref multi-group = true crosspost; Injection-Date minus Date = client clock skew.
- Other Haiku posting thread: ngPost GROUP_POLICY one-group-per-file explains the Batman split; session fingerprint = part length + line length + Message-ID shape + subject template. Its wave 2 covers Nyuu/ngPost/nxg-upper source, nZEDb collectionhash, PAR2 set ID across groups, and backbone injection headers.

### REVISED wave 2 plan (supersedes the reminder text, avoids overlap)
- A: uploader tools nobody has read yet: NewsUP, GoPostStuff, JBinUp, PowerPost/Camelsystem, Newspost, Postie, yencee, newsmangler forks (connection count, file/part ordering, retry behavior)
- B: article numbering behavior: how backbones assign numbers (feed batching, multi-connection interleave), cross-server number offsets, OVER ordering, plus burst and changepoint segmentation (Kleinberg, PELT) for splitting sessions
- C: repost culture: alt.binaries *.repost groups, request IDs, ReqBot/EFNet channels, scheduled repost bots, NZB <meta> and poster habits (timezone, cadence) from IRC logs, archive.org, forums
- Opus yEnc thread (06:50Z) already covered ngPost body quirks + Hydro74000 fork versions, GoPostStuff (UnixNano$gps@gopoststuff, uppercase pcrc32), newsmangler Message-ID leaking provider host, header order per tool, ngPost sends no Date. Its wave 2: PRNG state recovery from random IDs/From. So wave 2 A drops GoPostStuff/newsmangler and focuses on NewsUP, JBinUp, PowerPost/Camelsystem, Newspost, Postie, yencee, and posting *behavior* (connection count, retry, ordering) rather than header grammar.
- Opus posting thread (06:50Z): TRiToN split = ngPost --obfuscate + group_policy=each_file (postie too); random From still shares Message-ID domain; ngPost sends no Date so Date = server arrival. Arrival-curve join across groups via high-water-mark scans. DMCA co-removal vectors via STAT over time (rp.os3.nl/2014-2015/p82/report.pdf). So wave 2 B leans on backbone numbering mechanics and session segmentation math, not the arrival-curve idea itself; C stays on repost culture.
- Container thread (07:00Z): MKV DateUTC / _STATISTICS_WRITING_DATE_UTC give mux time, so mux-to-post latency is a per-uploader behavior signal. Fold into wave 2 C.

## Wave 2 (launched 07:17Z): A uploader posting behavior (NewsUP, JBinUp, PowerPost, Newspost, Postie, wrappers); B backbone article numbering + session segmentation; C repost culture + uploader habits (timezone via archive DOS time vs Date per archive thread)

### W2B result (article numbering + segmentation)
- RFC 3977 s6: numbers per group per server, follow arrival order; crosspost MAY get different numbers per group. Message-ID is the only cross-server key.
- Gnus nntp-xref-number-is-evil: Diablo multi-engine servers do not sync numbers, so Xref numbers vary by engine. Diablo feeder can master numbering (dreaderd -x); per-feed realtime/startdelay batching means peers see uploads at different times.
- RFC 4644 streaming pipelines and intermixes by Message-ID, so per-connection contiguity is not guaranteed.
- Segmentation: BOCPD (Adams & MacKay 0710.3742) via Rust augurs-changepoint 0.10.3 (BocpdDetector); PELT (1101.1438) offline; Kleinberg bursts for per-family counts.
- Takeaway: treat article numbers as a local arrival clock on one server (good for Atlas, which reads one provider at a time), session-split on arrival gaps with BOCPD, number runs as secondary.

### W2A result (uploader posting behavior)
- Postie (github.com/kipsilabs/postie, config-example.yaml, postie.kipsilabs.top/docs/configuration): group_policy default each_file; obfuscation_policy full (random filenames, subjects, dates, poster) vs partial (consistent poster); message_id_format random; article 750000; max_connections 10, inflight 10; retries 3 @5s, post_check max_reposts 1 after 10s, deferred check 30s backoff to 5m; watcher 5m, min_file_age 60s; wait_for_par2 true.
- NewsUP: article 768000 default, fixed From "NewsUP <NewsUP@somewhere.cbr>", par2 -s768000 -r15, connections 6-10.
- Upstream (isc-usenet/Upstream): -d sets Message-ID domain, which is a per-user fingerprint if left at its default.
- juicenet-cli (ParPar+Nyuu): reposts failed articles on the NEXT run, so a late repost of the same part number appears hours later.
- Unreached: JBinUp, PowerPost, yencee; NewsUP/Newspost source.
- Signals: segment size per tool (Nyuu 700K/716800, NewsUP 768000, Postie 750000); post-check repost events (same part/size reappearing 10s-5m later, or next run).

### W2C result (repost culture)
- Newsbin duplicate detector hashes ~first 26K of a file; fill check uses same name + ~600K prefix (forums.newsbin.com/viewtopic.php?p=213364, p=205548). Cheap prefix twin test.
- Crosspost seen in BinSearch NZB as the same part twice with different Message-IDs (forums.sabnzbd.org/viewtopic.php?p=46513), with X-Newsposter: newsmangler 0.02.
- SABnzbd dedupe is title + NZB link only. Takedown reposts are "whack-a-mole" (anecdote).
- Unverified: Spotnet RSA signing, timezone/cadence, IRC request bots, indexer admin write-ups.

### Wave 2 merge (07:20Z)
Lean in: Postie is a strong Batman-split candidate alongside ngPost (each_file default + full obfuscation). Post-check reposts (same part reappearing seconds to minutes later) are a session-link event. Segment size per tool. Session split via BOCPD on arrival gaps, numbers only as a single-server clock.
Dead: cross-server number mapping (numbers are not portable); IRC/req mechanics via search.
Wave 3 plan (07:45Z):
- A: read Postie (internal/), NewsUP (codeberg), juicenet source: retry Message-ID regeneration, PAR2 order, how "random dates" are drawn (a distribution is itself a fingerprint), connection scheduling.
- B: posting rate fingerprints: provider per-account upload speed caps, bytes/sec per connection, Date vs NNTP-Posting-Date / Injection-Date skew, backbone arrival skew measurements.
- C: pivot: Spotnet/Spotweb signed spots (identity keys), nZEDb/newznab forum archives on repost/duplicate handling via archive.org, X-Nxg style metadata headers.
- yEnc forensics wave 2 (07:20Z): Date string format clusters tools (-0000 Python formatdate, +0000, GMT, UTC); PowerPost subjects lack 'yEnc'; posted NZB writer fingerprint (iso-8859-1 newzBin DOCTYPE vs Nyuu utf-8); shared password meta = same uploader. X-Received-Body-CRC undefined publicly.
- Other Haiku posting thread wave 2 (07:21Z): ngPost hands files out sequentially, so file k's tail overlaps file k+1's head in time across connections (stitch key across groups); ngPost retry = new UUID immediately; Nyuu post-check repost keeps Message-ID+Date, >=35s later; nxg-upper Message-ID = SHA256('NxGHeader:type:number') deterministic; clock fingerprint = Date - RAR/zip DOS time. Their wave 3: STAT probing of predictable IDs, record-linkage/sessionization, real header dumps. My wave 3 keeps to posting-rate, Postie/NewsUP internals, Spotnet + indexer archives.
- Opus yEnc wave 2 (07:24Z): Nyuu-Obfuscation uses ~111 V8 Math.random draws per article (xorshift128+ state recovery, unverified); ngPost From from 32-bit seeded std::rand; NewsUP fakes 7 tools' Message-ID grammars but yEnc layer gives it away; postie vs Nyuu-Obfuscation: Date suffix +0000 vs GMT backdated 0-6h, header order random vs fixed. Wave 3 A should confirm Postie's date randomization range from source rather than re-derive grammar.
- Opus posting wave 2 (07:28Z): per-group bursts tile (end of file k ~ start of k+1, ~2 article overlap per connection); fill tells: nzbrefresh keeps Message-ID but resets Date; nzb-repair uses part>1 with begin=1. Their files: research/posting-behavior/wave-02-*.md (same folder, separate names).

> 07:28Z: the owner shared 'Linking Signals Map' (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE) as the output-format reference: each signal tagged by what it proves (twin, alt, same film, same uploader, posting tool), read cost tier (overview row ... every article), and Atlas status. Final report should use that shape and fill its gaps: NewsUP Message-ID (fakes others), JBinUp/PowerPost unknowns, session-split method, repost events, posting-rate signals.
- yEnc Haiku wave 2 (07:29Z): Highwinds injects Path 'fxNN.iad.POSTED!not-for-mail' (injecting front-end id = per-session clue for wave 3 B); Usenet.Farm X-Received-Body-CRC decimal uint32; no mainstream poster sets References/Cancel-Lock.

## Wave 3 (launched 07:48Z): A Postie/NewsUP/juicenet source; B provider posting rates, injection headers, clock skew; C Spotnet signing, indexer admin archives, NZB meta

### W3B result (posting rate, injection, skew)
- Eweka allows 50 connections; user reports: upload.eweka.nl nyuu 14 conn ~20 MB/s, 40 conn ~51 MB/s; Tweaknews ~100 Mbit cap (anecdotal r/usenet l38e4d). No per-provider posting caps published.
- INN nnrpd Injection-Info: posting-host="host:ip"; logging-data="<pid>" (per nnrpd process = per connection); posting-account optional (INN 2.6 addinjectionpostingaccount). Injection-Date added on POST if absent. (lists.isc.org inn-workers 2009-12/017367, 2014-11/018334). logging-data is per session per IETF usefor.
- Date format anomalies are posting-agent signatures (Gierth, 14.5M headers, inn-workers 2002-09/010390).
- Clock skew: Polcak et al JUCS 21(9) — skew via timestamp-offset slope, converges in minutes, not unique across hundreds of hosts. Weak evidence only.
- Gap: fxNN Path token stability unverified; big backbones may strip Injection-Info (needs live HEAD check on prod).
- Key: logging-data pid = one NNTP connection, strongest HEAD-only session link IF backbones keep it. Atlas can check this on its own prod HEADs.

### W3C result (Spotnet, archives, NZB meta)
- Spotnet spot = headers on an article in free.pt: From `Nick <USER-MODULUS.USER-SIGNATURE@CAT KEYID SUBCATS.SIZE.x.DATE.CUSTOM.SERVER-SIG>`, X-XML (signed by X-XML-Signature), X-User-Key RSA modulus/exponent. XML <NZB><Segment> = Message-ID of a zipped NZB in alt.binaries.ftd. (github.com/spotnet/spotnet/wiki/Spot-Header-format, Spot-Xml-format)
- So: RSA modulus = per-uploader identity (to verify on real spots), and spot -> NZB -> article Message-IDs is a ground-truth label set for training/validating session links.
- SABnzbd forum 2011 (p=58021): proposed hashing the NZB's Message-ID set as an identifier. NZB meta types: title, tag, category, password (SABnzbd nzb-spec); poster only per <file>.
- Not found: admin threads on collection hash / multigroup stitching.

### W3A result (source code, via summarizer, verify before relying)
- Postie internal/poster/poster.go: RandomDateWithinLast6Hours = now - U[0, 6h) per article (local zone), under full obfuscation; GenerateFrom `{14} <{14}@{5}.{3}>` crypto/rand, new per segment under full; GenerateMessageID `{32}@{8}.{3}` only in random mode, else nxg GenerateSegmentID(partType, partNumber); RETRY KEEPS the Message-ID (corrects my wave 2 note); crash recovery reuses manifest IDs; postPipelineDepth 2 (two files concurrently); each_file group = rand.Intn per file.
- NewsUP bin/newsup.pl: --obfuscate picks a RANDOM SUBSET OF GROUPS PER ARTICLE (true per-article spray), From empty, IDs from NewsUP::Utils generate_random_ids; IO::Select multiplexer.
- juicenet-cli: resume file; Nyuu dump-failed-posts raw articles reposted at the start of the next run (same Message-IDs) via repost_raw.

### Wave 3 merge (08:05Z)
- Under Postie full obfuscation the Date header is noise (uniform 6h backdate per article); only server arrival (Injection-Date / article number) carries timing. Date dispersion itself (spread up to 6h, local zone suffix) fingerprints Postie.
- NewsUP obfuscate is the worst case: one file sprayed over a random group subset per article. Linking must come from yEnc name/size/begin-end + arrival time across groups.
- Injection-Info logging-data (nnrpd pid) is a per-connection key IF backbones keep it; Atlas can check on its own HEADs.
- Spotnet spots give ground-truth uploader keys and spot->NZB Message-ID mapping.
Wave 4 plan (08:15Z):
- A: lean: stitching per-article spray (NewsUP) and per-file spray (Postie/ngPost): any indexer/search engine that merges across groups (binsearch "other groups", nzbindex, NZBKing), Nyuu dump-failed-posts/Message-ID reuse in source, NewsUP Utils.pm generate_random_ids.
- B: pivot: DMCA takedown notices as ground truth (Lumen database Usenet notices list Message-IDs per work; BREIN, Remove Your Media, NFOrce) and co-removal timing.
- C: pivot: validation datasets: public Usenet header archives / NZB corpora / binary group statistics (archive.org, giganews-era dumps, NZB corpora on GitHub test fixtures) we could test linkers against.
- Other Haiku posting thread wave 3 (07:53Z): ngPost group per file = std::rand()%n seeded by srand(QUuid.data1) per run; ngPost code default is ALL (sample config EACH_POST), so each_file default is only Postie. STAT by Message-ID ignores group (RFC 3977 6.2.1). Real case: RARs in a.b.misc, PAR2 in a.b.moovee (forums.newsbin.com p=220769). No public header dumps. Their wave 4: Spotnet ground truth, NZBs posted inside Usenet as manifests, PAR2 as cross-group bridge.
- REVISED wave 4 C (avoid overlap): drop validation datasets; instead propagation/completion monitoring: tools and sites that measure per-backbone arrival delay and completion (nzbcheck-style tools, provider completion stats), to size the slice-merge window.
- yEnc forensics wave 3 (07:54Z): server-assigned Message-IDs like NzZUA.117908$rY1.16813@fx45.am4 then OzZUA.117909$... (Highwinds counter +1 per article = same-session sequence from overview; they decode in wave 4). Nyuu raw-dump reposts keep ID + genTime. My wave 4 B stays on DMCA notice CONTENT (Lumen) not STAT co-removal (covered via UvA report).
- Opus yEnc wave 3 (07:55Z): V8 Math.random state recovery TESTED (one Nyuu-Obfuscation article predicts session's random headers); Postie crypto/rand cannot be linked that way; X-Received-Body-CRC stamped at injection; newspost/yEnc32/PowerPost send no Message-ID (server assigns, so counters apply).
- yEnc Haiku wave 3 (07:59Z): Date zone form = tool/version (GMT Nyuu, 'UTC' postie v0.0.24, '+0000' postie v0.0.35, none ngPost, local abbrev nxg-upper); Usenet.Farm adds X-Ufhash. Fetch tip for agents: proxy.golang.org and registry.npmjs.org serve source when GitHub is blocked.
- Opus posting wave 3 (08:03Z): ngPost file order QDir::Name (par2 index, rars, vol par2s); arrival reorder width ~2N estimates connection count N; Nyuu (msgid ms - arrival) drift = Kohno clock-skew fingerprint; Timeprints (Johansson et al., Security Informatics 2015) 24x7 histograms as tie-breaker only.

## Wave 4 (launched 08:26Z; 08:15 reminder had not arrived): A stitching sprayed posts (NNTmux multigroup, binsearch, NewsUP Utils.pm); B DMCA/NTD notices as ground truth (Lumen); C propagation delay, post-check timing, indexer hold windows
- Payload Haiku wave 4 (08:25Z): PAR2 exponent-0 recovery slice = XOR of all input slices, rename/order/tool-proof twin key from one article of vol 1.
> 08:27Z: per team memory, cross-thread messages are capped at 40/hour. From wave 4 on, share only with the Opus posting thread, the Haiku 10-min posting thread, and the catalog thread; everything else goes in linking-findings.md.

### W4C result (propagation and windows)
- No measured backbone-to-backbone delays published. Easynews: minutes to hours. NZBGet users set propagation delay 30-60 min.
- Nyuu post-check defaults (help.txt): check-delay 5s, check-retry-delay 30s, check-tries 2, check-post-tries 1 (repost once). Postie issue #184 5s check delay; PR #286 10s test delay. ngPost retry=5 on post failure, nzbCheck off.
- Hold windows: newznab 4h hold for incomplete releases; newznab-tmux forks delaytime 2h (8h prod), collection_timeout 48h, partretentionhours 72-144h, late-header grace 72h (KurzonDax/newznab-tmux-kd #464, #1020); xbmc4lyfe/newznab-tmux PR #15 collection_delay_hours 2.
- Suggested policy (inference): merge complete pieces immediately; hold incomplete slices 4h for cross-group/repost merges with 30-60 min rechecks; hard expiry 72h. Never purge fragments (Atlas rule: late articles join the older release).

### W4B result (takedowns)
- Notices list Message-IDs per article (UvA report p.5). Removal is "all or nothing" per fileset (presentation); no removals in first 18h, average ~25h Eweka, ~95h Astraweb (2015, EFNet subset, p.24). Same-parent providers share takedowns (r=1.00 pairs); Giganews diverges.
- Lumen schema has no Message-ID/newsgroup fields; no Usenet notices found publicly. Dead end as a corpus.
- Rights-holders find posts via NZB indexers (Tom's Guide 2012, UFC Bayesian classifier), so takedown sets follow indexer releases.
- Use: Atlas can STAT its own catalog over time; co-removal within one window = same release (validation label), and a sliced release whose slices vanish together confirms a stitch.

### W4A result (stitching)
- NewsUP Utils.pm generate_random_ids: newsup format "$sec$usec.$rand@$time.newsup"; under OBFUSCATE also mimics JBinUp "<32>@JBinUp.local" and Nyuu "<12>-$sec$usec@nyuu". Article.pm: random 8-char From when unset; obfuscated subject = random 1-32 lowercase; yEnc name= keeps the REAL filename ("TODO: to be truly obfuscated we need to break YENC spec").
- CONFLICT: W3A (newsup.pl via summarizer) said random group subset per article; W4A found no random-group code in Utils.pm/Article.pm. Treat as unconfirmed.
- No indexer (NNTmux, nZEDb READMEs) documents cross-group stitching. Newsbin search recovers PAR2 across groups but not randomized RARs (forums.newsbin.com p=220769).

### Wave 4 merge (08:32Z)
- Stitch key across groups: yEnc (name, size, total, part, begin/end) + pcrc32; names survive in NewsUP. Arrival window 4h hold, 72h hard expiry; recheck 30-60m.
- Takedowns: Message-ID based, whole fileset removed together, 18h+ lag; useful as Atlas's own validation (STAT over time), not as a public corpus.
Wave 5 plan (08:45Z):
- A: resolve NewsUP group-spray conflict from bin/newsup.pl; Postie docs/obfuscation; JBinUp and PowerPost behavior (still unknown).
- B: pivot: uploader personas: recurring subject tags and habits of prolific Usenet uploaders (e.g. TRiToN, "Old but Gold", reupload bots), how indexers tag posters, and per-uploader posting signatures.
- C: pivot: what Atlas's own provider exposes: Eweka/Omicron header and overview specifics (Path tokens, Injection-Info, X-Received-*, Xref form, server-assigned IDs), from forum header samples.
- Other Haiku posting thread wave 4 (08:28Z): Postie full obfuscation gives EVERY ARTICLE its own random yEnc name (GenerateRandomFilename per segment, poster.go:1447), so the yEnc-name stitch key fails for Postie; use size + begin/end + pcrc32 and the PAR2 16k-MD5 bridge (first article's first 16384 bytes -> PAR2 FileDesc). PAR2 creator "Postie". nzb-monkey-go directsearch builds NZBs from headers by group+date. TIP: shallow git clone works for subagents when GitHub API/raw is blocked. Their wave 5: what survives full obfuscation + TRiToN case.
- Wave 5 B adjust: uploader personas EXCLUDING TRiToN (theirs).
- Opus yEnc wave 4 (08:33Z): NewsUP random Message-ID style per article from one drand48 stream; its own style leaks 48-bit state from one ID (session-wide prediction). ngPost Windows (MinGW) <v4.15 seeds rand with ms timestamp. Overview :lines sequence as twin hash (modelled).
- Opus posting wave 4 (08:36Z): simulated cross-group join needs only seconds ([-1.5,+0.5]s around file boundaries), P .975 / R .942 per link (research/posting-behavior/sim/); 4h hold ample, 72h for fills. Says NewsUP random group subsets reveal the pool in ~10 articles (supports W3A). Long-term uploader matching: FP-STALKER upgrade rules (versions only go up), Li et al. VLDB 2011 decay weights.

## Wave 5 (launched 08:46Z): A legacy Windows posters (JBinUp, PowerPost A&A, Binpost, SpotPost...); B uploader personas (excluding TRiToN) + poster modelling; C what Eweka/Omicron and peers expose (Path, Injection-Info, server IDs, OVERVIEW.FMT)

### W5B result (personas): thin
- Timeprints (Johansson, Kaati, Shrestha 2015, link.springer.com/article/10.1186/s13388-015-0022-z): 24 hour bins + period/day-of-week, cosine; 100% top-1 at 500 aliases, 70.6% at 4,000; needs ~hundreds of posts per alias (<20% at 100 posts, >95% at ~800). Stylometry alone 44.6%.
- SpamSpotter: score senders by global sending behaviour (reputation, not identity).
- No indexer poster model or uploader crew catalogue found (nZEDb/NZBgeek fetches failed). Persona signals mostly inference: packaging fingerprint (RAR volume size, PAR2 redundancy, SFV/NFO presence, article size, line length), group set, cadence.
- Dead end-ish: persona catalogues via web search.

### W5A result (legacy Windows posters)
- Camelsystem PowerPost: X-Newsposter "Camelsystem Powerpost (Modified POWER-POST http://powerpost.camelsystem.nl)", Organization Camelsystem, Message-ID <part1of1.{20 alnum}@camelsystem-powerpost.local> (users told to change the domain), Date "01 Jun 2013 13:25:51 GMT" (no weekday), line=128. Fixture: github.com/radekstepan/nzb-grabber-js test/fixtures/articles/. Guide: post NFO/SFV first, PAR2 from QuickPar, one group per upload.
- yEncBin Poster: "Upload Individual Parts (Filling)" re-uploads only missing parts later (new Message-IDs); lines per part 1500/1950/3000/5000; multi-group checkbox crossposts.
- Easy Post: subject "Subject [a/b] - "file" yEnc (x/y)", 500 KB parts default, top-to-bottom order.
- JBinUp: sequential uploads, header check, unmaintained; jbinup.com domain now unrelated. Spotnet keys 384-bit RSA + hashcash (github.com/rousseauxy/spottr).
- Not found: Binpost, NewsLeecher, Easynews web posting, SpotPost.

### W5C result (provider exposure)
- No Eweka/Omicron/Highwinds header sample online. One Giganews 2016 sample (pastebin.com/LxCzQDvN): Path buffer1.nntp.dca1.giganews.com!border1...!goblin3!goblin2, Xref number.nntp.giganews.com, NNTP-Posting-Host IP, X-Trace "ts IP (date)", X-Received-Bytes, X-Received-Body-CRC decimal; no Injection-Info.
- Eweka claims its own NL backbone (not confirmed Omicron). XZVER: Usenet.Farm/Cheapnews yes, COMPRESS GZIP no; Astraweb/Blocknews both (forums2.newsbin.com p=222309).
- Conclusion: only a live check from Atlas (CAPABILITIES, LIST OVERVIEW.FMT, HEAD on a few binary articles) can settle which session signals Eweka exposes. Recommend in final report; no prod action taken here.

### Wave 5 merge (08:55Z)
- Legacy: Camelsystem PowerPost fingerprint (X-Newsposter + part1of1.{20}@camelsystem-powerpost.local); yEncBin "Filling" reposts.
- Personas: only Timeprints (needs hundreds of posts). Persona catalogues via web: dead end.
- Provider exposure: unanswerable from the web; needs a live HEAD from Atlas.
- Peer coverage check: Fellegi-Sunter + gated union-find (Rust ena) already in the Opus posting thread, so no linkage-method wave here.
Wave 6 plan (09:15Z):
- A: lean: PowerPost A&A / yEncBin / PowerPost 2004 source (SourceForge, GPL) for fill semantics (Message-ID reuse?), subject builder, Message-ID generator.
- B: pivot: overview arithmetic: what :bytes and :lines count per server (headers included?), so header length from fixed-length random fields becomes a per-tool/session fingerprint from XOVER alone; empirical article size distributions.
- C: pivot: niche ecosystems as ground truth: Anime Tosho (Usenet posts mirrored from torrents, by Nyuu's author; feeds with NZB + torrent mapping), ebook and music posting conventions.
- Payload Opus wave 5 (08:57Z): unencrypted RAR deterministic across runs/versions, so re-RARs share middle articles' yEnc bodies + pcrc32 (repost twin at article level); RAR5 -hp last volume = 14 mod 16; Anime Tosho crc32k torrent bridge (they cover the content key; my wave 6 C keeps to Anime Tosho's posting pipeline and feed mapping).
- Haiku posting wave 5 (08:58Z): Postie full: rapidyenc lowercase padded pcrc32, line=128, header order From,Newsgroups,Subject,Message-ID,Date ('UTC'); defaults 750000 and 768000 both appear (config.go:575, 1110). ngPost obfuscate keeps real name=, From @ngPost.com. NewsUP: no group spray in lib/NewsUP (but W3A and Opus point to bin/newsup.pl get_random_array_elements; still OPEN, wave 6 A settles from bin/newsup.pl). Survivor per-file key: (size=, total=, article size) + begin/end chain. Batman Knightfall = one 47.8 GiB MKV, slices are RAR/PAR2 volumes.
- yEnc forensics wave 5 (09:00Z): Highwinds server IDs T1.N$K.R@fxNN.site (658 real IDs): N = front-end-wide counter continuing across a post's files (gaps = other posters), K constant per SESSION -> group-independent session key + arrival clock from overview alone. Applies to posters that let the server assign IDs (newspost, sanguinews, newsmangler, nntpPoster, PowerPost 2004). CONFLICT: postie header order random (this thread) vs fixed From,Newsgroups,Subject,Message-ID,Date (Haiku posting thread).
- Opus yEnc wave 5 (09:05Z, re-implementations): NewsUP one .newsup ID -> drand48 state -> all sibling IDs + From + Perl srand seed; Nyuu-Obfuscation predicted 60/60 neighbours; ngPost ~200s/32 cores per From; Postie crypto/rand safe. Code: research/yenc-header-forensics/opus/rng-tools/.

## Wave 6 (launched 09:21Z): A NewsUP spray settle (git clone) + PowerPost fill source; B overview :bytes/:lines arithmetic; C Anime Tosho pipeline as ground truth
- Payload Haiku wave 6 (09:21Z): Sonarr Parser.cs re-uploader suffix dictionary (-Obfuscated, -Scrambled, -Rakuv, -WhiteRev, -BUYMORE, -AsRequested, -postbot, -xpost) = uploader persona tags (fills W5B gap).

### W6A NewsUP spray SETTLED (verified myself from clone, github.com/demanuel/NewsUP @43a4665, 2020-08-20)
- bin/newsup.pl:331-336: with OBFUSCATE, every article gets `newsgroups => get_random_array_elements($options->{GROUPS})` and `from => ''`; Message-IDs from generate_random_ids per file.
- lib/NewsUP/Utils.pm:348-360: Fisher-Yates shuffle of the group list IN PLACE (state persists across articles), then returns the first 1..n groups (count = int(rand(n))+1). So each article goes to a random non-empty subset, crossposted (one Message-ID, several groups in Xref).
- Implication: NewsUP-obfuscated articles are true crossposts with varying group sets; union of Xref groups over ~10 articles reveals the configured pool (matches Opus). Stitch on yEnc name= (real name kept) + size + begin/end.
- W6A PowerPost source: not attempted (agent spent its run on NewsUP). NewsUP Utils.pm ~774 imitates PowerPost IDs: part<N>of<M>.<22>@camelsystem-powerpost.local or @powerpost2000AA.local, i.e. the partNofM prefix (real PowerPost) encodes part/total in the Message-ID. Codeberg blocked by egress proxy. Carry PowerPost source to wave 7 A.

### W6C result (Anime Tosho)
- animetosho.org/about: mirrors torrents to file hosts and Usenet; feeds RSS/ATOM/JSON and Newznab/Torznab at feed.animetosho.org/api with no API key; daily dbexport at storage.animetosho.org/dbexport/ (files: crc32, sha1, ed2k; torrents: stored_nzb, AniDB gids). Dead links are not re-uploaded; >16GB torrents skipped.
- Posting pipeline details (groups, From, article size) NOT documented. No Message-IDs or post times in the export docs.
- Use (inference): its stored NZBs are labelled releases whose article sets we can compare with what Atlas observes; torrent time vs NZB time gives one bot's posting cadence.

### W6B result (overview arithmetic)
- RFC 3977 s8 text not reached (fetch truncation); :bytes semantics unverified. NZB segment bytes source undefined (SABnzbd spec: "size of the article").
- yEnc escape ratio for random data at line 128 ~0.0158 (node-yencode README); SD of escapes ~105 bytes per 716800-byte article.
- Inference: H = :bytes - [S(1+e) + 2*ceil(S(1+e)/128) + Y]; noise ±~105 B, so only coarse header-length bands (tens of bytes, e.g. long random From/Message-ID vs short) are separable. Weak overview-only signal; :lines already carries escape density at 128-byte granularity.

### Wave 6 merge (09:30Z)
- NewsUP per-article random crosspost subset VERIFIED (bin/newsup.pl:331-336, Utils.pm:348-360).
- Anime Tosho: no-key Newznab feed + daily dbexport + stored NZBs = public labelled releases.
- Overview header-length fingerprint: weak (escape noise).
- PowerPost source: still open.
Wave 7 plan (09:45Z):
- A: carry-over: PowerPost 2004 (sourceforge, GPL) / A&A / yEncBin source: Message-ID generator (partNofM.<rand>@...), fill semantics, subject builder.
- B: lean: server-assigned Message-ID formats beyond Highwinds: INN's own generator (what it encodes: time, pid, sequence), Diablo/Typhoon/Cyclone, Giganews, UsenetExpress, Abavia, Eweka; which encode a session/counter.
- C: pivot to EMPIRICAL: download a few public NZBs (Anime Tosho stored NZBs, no key) and measure: per-file <file date> gaps, segment bytes distribution, Message-ID shape, poster, groups; check tile/boundary timing and article size vs tool defaults. Python -I in scratchpad.
- Payload Opus wave 6 (09:28Z): packer defaults per tool: ngPost --compress rar -ep1 -m0 -v42m no password (nzbup bot same); NxG Upper -hp; postie raw files (no packing, full obfuscation); Nyuu/GoPostStuff no packing; NewsUP 7z -mx0 -v50m. Volume size (42m vs 50m) = tool fingerprint from NZB sizes.
- Haiku posting wave 6 (09:29Z): overview :lines gives article size and per-file size to ~±300 B (simulated); Spotweb source: spot NZB Segment IDs point to DEFLATE-compressed NZB carriers in a.b.ftd, unescape =C/=B/=A/=D then gzinflate -> ground-truth per-file groups + segment IDs. They corrected NewsUP notes.
- yEnc forensics wave 6 (09:30Z): Highwinds ID key (host, K, T1 window, N range) groups one upload session from XOVER; K constant per post, T1 = injection second; N interleave 0-27% = concurrent uploads. Wave 7 B excludes Highwinds and covers other servers' generators.
- Opus yEnc wave 6 (09:41Z): stock Nyuu Message-IDs RNG-solvable on REAL 2025-26 posts (two IDs recover state, up to 906 IDs linked into one stream); Nyuu ms timestamps place orphan files into their upload 2565/2565. Old NewsUP (2017) IDs '32 letters@UFK'.

## Wave 7 (launched 09:47Z): A PowerPost source; B server-side Message-ID generators (INN etc., not Highwinds); C empirical measurements on public NZBs (Anime Tosho, test fixtures)
- Payload Haiku wave 7 (09:49Z): Cleanfeed EMP rejects identical raw bodies after a few repeats within ~2h (MD5, binaries included). Behavioral implication: quick reposts/crossposts-as-separate-articles must alter the body (yEnc name=/header line), which explains per-article name randomisation; true byte-identical article twins are more likely when hours apart or via Xref crosspost (one article).

### W7A result (PowerPost 2004 real source, SourceForge CVS snapshot sourceforge.net/code-snapshots/cvs/p/po/powerpost.zip)
- No Message-ID and no Date sent ("let the server fill it in", PowerPostThread.cpp ~325): server-assigned IDs (Highwinds K/N session key applies).
- From "Name <email>" (default "Sam Adams <Sam@Adams.com>"); crosspost = one article, comma list no spaces; subject template "$F (n/N)", zero-pad to digits of total, part 0/N text article; part count = size/(lines*128)+1 (exact multiple gets an extra part — a quirk visible from sizes).
- Retries resend the same part, same subject; server assigns a new ID.
- Camelsystem part1of1.<rand>@camelsystem-powerpost.local is from the A&A fork, not PP2004 (source not found).

### W7B result (server-side Message-ID generators)
- INN lib/messageid.c:46-72: `<Radix32(time-673416000)$Radix32(pid)$count@domain>`, alphabet 0-9a-v; nnrpd 340 reply suggests it. Verified on 2004 example c02u1r$o4h$1@testnews.in-ulm.de -> 2004-02-07 14:51:39 UTC, pid 24721 (matches syslog). Fork-per-connection: pid = one NNTP connection, count consecutive => session key + order. Prefork: counter spans connections.
- Astraweb-style `hex_time$0$counter$c3e8da3$8hex@news.astraweb.com` (nzbget tests/testdata/nzbfile/plain.nzb): hex Unix time first; constant middle field maybe server/session key (3 samples).
- big-8 sample <hg3q9m$2p2$1@news.albasani.net> = INN form (decodable).
- No primary formats found for Giganews, UsenetExpress, Abavia, Eweka, Usenet.Farm, XSNews, Easynews.
- (Checked: subagents' only device_bash calls were no-ops 'echo skip'/'true'; nothing ran on the Mac.)

### W7C result (measured on public NZB fixtures; Anime Tosho blocked by egress)
- Sources: github.com/sanderjo/NZBs (16 Nyuu fixtures in alt.binaries.test + 1 real NewsUP 2017 post), sabnzbd test files. Scripts copied to research/posting-behavior/haiku15-tools/ (nzb_stats.py, nyuu_check.py).
- Nyuu 716800 raw -> NZB segment bytes mode ~728.1-739.7K (escape/content dependent, ~1.6-3.2%); real NewsUP post mode 793076 (768000 raw +3.3%), IDs 32 random @UFK (matches Opus's old-NewsUP form).
- <file date> is not monotone in 7/17 (jitter up to -105s); Nyuu ms-stamp vs file date skew 0-93s. Real post: 46 files over 25 min, gaps 0-120s in batches.
- Takeaways: date-sorting needs ~2 min tolerance; encoded size alone is weak, Message-ID shape is the strongest tool fingerprint; NZB file date is a per-file stamp, not an article clock.

### Wave 7 merge (10:05Z)
- Server-assigned IDs are decodable session keys: INN `<time32$pid32$count@domain>` (pid = connection, count = order), Highwinds (yEnc thread), Astraweb-style hex-time `$` form. PowerPost 2004 and newspost leave IDs to the server.
- Real data matters most now; the richest source is Atlas's own header log on prod (recommend in final report, no prod action here).
Wave 8 plan (10:15Z):
- A: empirical on real multi-group posts: public no-key NZB downloads (nzbindex, binsearch, nzbking) for recent obfuscated posts; measure spray, dates, ID shapes with haiku15-tools scripts.
- B: decode the `$` (hex_time$0$counter$const$hex) family: which server software (Typhoon/Cyclone/Twister), more samples from public NZBs, what the constant and counter mean.
- C: pivot: uploaders posting through several providers/servers at once (Nyuu/ngPost multi-server configs, backup servers): effect on server-assigned keys, Injection hosts and arrival order within one session.

## Wave 8 (launched 10:18Z): A real multi-group NZBs; B $-style server IDs; C multi-provider posting sessions

### W8B result ($-style server IDs), partial
- 4 full Astraweb IDs from nzbget/gonzbee test fixtures, e.g. 4f9a3e8a$0$14349$c3e8da3$33a0879d@news.astraweb.com.
- Field 1 = hex unix time, equals the NZB file date exactly (2 of 3); r00 is 13 s off (probably per-segment time).
- Field 2 = "0" constant. Field 3 = counter, NOT monotonic (two IDs in the same second: 26527, 23145), so per-process or random.
- Field 4 = c3e8da3, constant across Jan-Apr 2012; as an IPv4 it reads 12.62.141.163 (owner unverified). Likely per-host, NOT per-session.
- Field 5 = 8 hex, differs every ID; maybe a hash, untested.
- Older kabelfoon.nl IDs share the shape (41856a4d$0$251$58c7...), so it is server software, not Astraweb-specific. Software not identified.
- Takeaway: on $-style IDs, only field 1 (time) is usable; fields 3-4 are not a session key on this evidence. Downgrade from "session key" to "server fingerprint + timestamp".

### W8A result (real multi-group NZBs): target not met
- nzbindex, binsearch, nzbking all blocked at the proxy (CONNECT 403). No 2023-2026 real NZBs reached.
- Only real posts found: sanderjo/NZBs Nyuu test posts (2018, 2020, 2022), single group a.b.test, poster blablamannetje, IDs `<24 letters>-<13-digit ms>@nyuu` (and `@nothereatall`, a custom domain). Confirms the Nyuu ID family; custom domains exist so match on the `-\d{13}@` shape, not the `@nyuu` suffix.
- ID ms vs NZB file date: within 1 s for 151/164 segments in one file, far fewer in the others (skew up to 26 s). NZB file date is one value per file, so later segments drift; compare ID time to each article's own Date, not the file date.
- All other NZBs were synthetic parser fixtures. Spray across groups on live data remains unverified from public sources.
- Takeaway: public indexers are unreachable from here. The only way to measure real spray is Atlas's own header log on prod (recommend to the owner).

### W8C result (multi-provider sessions)
- ngPost: one job-wide queue, all connections across all [server] blocks pull from it (Poster.cpp:85-89), failures re-queued (Poster.cpp:121, NntpConnection.cpp:195/235/401). Verified.
- Nyuu: single --host; second provider = second run. Verified (help-full.txt, lib/uploader.js:79).
- Postie: nntppool DispatchRoundRobin over all servers (config.go:958/975/990); primary/backup tiers with 430 fallback from pkg.go.dev summary only.
- Inference: each TCP connection sticks to one front-end; a post yields many (host, fxNN) key streams.

### Wave 8 merge
- Lean: source reading works, live indexers don't. Server-assigned "session keys" are per connection, not per post; multi-server posters fan out across many.
- Downgrade $-style IDs to timestamp + server fingerprint.
- Wave 9 plan: A nntppool source (dispatch, tiers) + Postie Message-ID and header-order code (settle the header-order conflict). B ngPost ArticleBuilder Message-ID + NewsUP multi-server/FAQ + Nyuu wiki. C identify $-style ID software (Diablo/DNews/Cyclone source and docs, RDAP for 12.62.141.0/24) and Highwinds T1.N$K per-connection behavior.
- Peer (Haiku 10-min posting thread, 10:22Z): no overlap with wave 9. W9A must answer which header-order path Postie ships today: nntppool's fixed-order live path vs article.go's map order (their wave 5 open conflict). Their wave 8 covers HDR/XHDR/OVERVIEW.FMT/XZVER/COMPRESS per provider and evasion features.
- Peer (Haiku payload thread, 10:23Z): par2cmdline and ParPar default recovery exponent 0, so the exp-0 PAR2 slice key needs only matching slice size; ngPost makes PAR2 only with --par2_pct. No overlap with wave 9.
- Peer (Opus payload thread, 10:30Z): encrypted reposts keep identical volume sizes whatever the password; (V, n, S_last) is a ~27.5-bit blocking key, plus one exact file size gives <1 false pair per 1e8. Disc posts: AACS Unit_Key_RO.inf SHA-1 / UDF Volume Set ID as per-master keys. No overlap with wave 9; relevant to final report as the payload-side complement to timing.
- Peer (Haiku 10-min posting thread, 10:30Z): Postie sizes from source 750000 default / 768000 preset (config.go:574-575, 1110). ngPost GROUP_POLICY default ALL; Postie defaults each_file. Postie PAR2 FileDesc keeps the real basename although files are renamed (internal/par2/par2.go:107-113): name leak. No shipped uploader randomizes article size, line length, order or jitter (Nyuu #121). Postie Date is randomized 0-6 h per article, so timing for Postie must come from server arrival order / article numbers / server IDs, not Date. Their wave 9: server-side ordering and injection headers as the Date replacement (overlaps W9C slightly; keep W9C on $-ID software and Highwinds per-connection only).
- Peer (Opus yEnc headers thread, wave 7, 10:30Z): Nyuu ID is re-randomized at send time (randomizeMessageID), so 48-draw gaps (72 = retry). 2017-2022 Nyuu posts on Node <=11 (V8 < 7.1) use a nonlinear output, so the RNG solver fails on those. `[A-Za-z]{32}@UFK` is SERVER-assigned per them (CONFLICT with my earlier note that old NewsUP IDs look like that; resolve in final report: probably NewsUP omitted the ID and the provider assigned it). `<32hex>@ngPost` is uuid4. Deterministic linkable IDs: geri (HMAC, crc32 in domain), keimpema/Usenet `part{n}of{N}.{guid}@unknown.com` (one GUID per file), Upstream microsecond time `@riverup`.
- Peer (payload-fingerprints, wave 9, 10:42Z): article pcrc32s fold to the same whole-file CRC32 as PAR2 IFSC slice CRCs (tested), so one PAR2 article links uploads with no payload. ngPost default: no PAR2, 716800-byte articles (same size as Nyuu, so 716800 alone does not identify Nyuu). False-link measurement: Splink cluster metrics, arXiv 2404.05622 sampled-cluster estimator (use in final report's validation section).
- Peer (payload-fingerprints Haiku 15, 10:46Z): FINISHED. Final report https://claude.ai/artifact/4encPYR1gKbgmAU9vQQ76g (research/payload-fingerprints/haiku15/final-report.md). Cross-reference it from my final report rather than repeating payload signals.

## Wave 9 (launched 10:51Z): A nntppool dispatch + Postie header order/ID; B ngPost ArticleBuilder + NewsUP multi-server/UFK + Nyuu order; C $-style ID software + Highwinds K scope
- Peer (Haiku payload wave 9, 10:52Z): par2cmdline default slice = ceil(total/2000) rounded to x4 over ALL files, so an extra file breaks the exp-0 PAR2 key. srrDB archive-crc search names obfuscated scene reposts from 1 article (robots-disallowed; sparing use). Payload-side; cross-reference only.
- Peer (Opus yEnc headers wave 8, 10:53Z): Nyuu on Node 12+: 19/19 NZBs link 100% of IDs to one RNG stream (2 IDs enough), but each NZB is its own process, so 0 cross-NZB links even 3 min apart. Old Nyuu (Node <=11) unsolved. Consolidated table: research/yenc-header-forensics/opus/w8c-fingerprint-table.md (33 tool variants, 6 server ID families, 9-step classifier, 21 conflicts) - use as the fingerprint table source in the final report. CONFLICT with my wave notes: they say Nyuu redraws the 24 letters on every send including retries, only the ms suffix survives; I had "post-check repost keeps the ID". Final report: say the ms suffix survives, the letters may not.

### W9A result (nntppool + Postie), spot-checked by me
- Postie pins nntppool v4.22.3 (go.mod:14). Shipping path poster.go:1582 -> postYenc (reposter.go:25-74) -> nntppool PostHeaders.WriteTo (post.go:168-236). FIXED header order: From, Newsgroups, Subject, Message-ID, Date, then Extra keys sorted (X-Nxg, custom). Postie's article.Encode (map order) has no non-test caller: dead code. CONFLICT RESOLVED: Postie header order is fixed. (I confirmed the WriteTo order myself.)
- Dispatch: one main provider chosen per article by weighted round-robin (weight = available connections, scaled by speed score; nntp.go:2817-2839, 3015-3026). Postie sets no backup tier. Status replies (440/441/430) are not retried; only transport errors move to the next server. Postie-level retry (3x) on stale-connection errors re-runs dispatch, so it can land on another provider.
- Message-ID, From, Date fixed before dispatch (poster.go:1410-1425); retries keep all three. Default ID `{32}@{8}.{3}`; nxg format is SHA256-derived `40@21.3` (nxg.go:48-59, 104-122). X-Nxg encrypted per-file header when AddNXGHeader and not full obfuscation. Date = now, or random within 6 h under full obfuscation.
- No per-server header differences. Inferred risk: cross-server retry reuses one pipe, possible truncated article under the same ID.

### W9C result ($-style IDs = Diablo dreaderd), verified by me
- Diablo dreaderd/subs.c GenerateMessageID: `<%08lx$%d$%d$%x@%s>` = time_hex $ per-second counter (static per process, resets each second) $ getpid() $ shash(vs_HostName) @ vs_ClusterName. I read the source and re-ran the shash port: news.astraweb.com -> 0c3e8da3, news.kabelfoon.nl -> 58c7af7e, news.xs4all.nl -> e4fe514c (all match real IDs). The 12.62.141.163 IP decode in W8B was a coincidence.
- CORRECTION of W8B: field 2 ("0") is the per-second counter; field 3 is the dreaderd PID, which is why it is "non-monotonic". So the shape DOES carry a process key: (host hash, pid) = one dreaderd process; the counter orders posts within one second in that process. Weaker than INN nnrpd pid (one process per connection) because dreaderd forks a pool of processes that each serve many connections. Use (hash, pid, time window) as a soft session key; two IDs in the same second with different pids = different processes.
- Astraweb's extra 5th field (8 hex) is not in stock Diablo; local fork or injector addition, unknown meaning.
- Highwinds T1.N$K.R@fxNN shape: not investigated, no samples.
- shash has suffix collisions (dreader.news.xs4all.nl = news.xs4all.nl); fine for host fingerprinting.

### Wave 9 merge
- Lean: source reading resolved two conflicts (Postie header order = fixed; $-IDs = Diablo with PID). 
- Wave 10 plan (last wave, fill final-report gaps): A Highwinds/Usenetserver/Newshosting fxNN ID samples + meaning of K (and any Diablo fork with a 5th field, e.g. Astraweb/Readnews); B ngPost ArticleBuilder + NewsUP UFK + Nyuu order (W9B rerun if it fails); C validation methods for timing-based stitching (false-link rates, Splink cluster metrics, arXiv 2404.05622) applied to session splitting.

### W9B result (ngPost, NewsUP, Nyuu), spot-checked by me
- ngPost: Message-ID `<QUuid hex@{msg_id signature, default ngPost}>` (NntpArticle.cpp:136-150), configurable domain. A retry gets a NEW UUID (tryResend, NntpArticle.cpp:117-124; verified by me), up to 5 tries; reconnects also regenerate. Header order From, Newsgroups, Subject, Message-ID; no Date. Obfuscated From re-randomized on every write (randomStdFrom per header() call). Server's 240 reply ID parsed back. Headers do not vary by server.
- W9B claimed __RELEASE_ARTICLES_WHEN_CON_FAILS__ is never defined: WRONG, it is defined in src/ngPost.pri:16 and src/ngPost_cmd.pro:30 (checked). So W8C's "failed articles return to the shared queue" stands.
- NewsUP: single upload server, flat file-order article array over IO::Select sockets. Without GENERATE_IDS/OBFUSCATE it sends NO Message-ID and the server assigns one (newsup.pl:344/384, Article.pm:268-283). Own generator `<sec><usec>.<base36>@<base36 time>.newsup`; obfuscate pool imitates JBinUp/JBinDown/Nyuu/camelsystem. No `@UFK` in NewsUP source: UFK is provider-assigned (agrees with the Opus yEnc thread; my earlier note was wrong).
- Nyuu: CLI single --host; engine supports opts.servers via a JS config (uploader.js:72-76). FIFO queue size min(round(conns*0.5)+2, 25); no ordering across connections, so parts finish out of order; post-check reposts via reloadQueue land later and out of order.
- Wave 10 plan (revised, final wave): A Highwinds `T1.N$K.R@fxNN` samples + meaning of K, and the Astraweb 5th field (Diablo forks). B validation of timing-based stitching: false-link measurement (Splink cluster metrics, arXiv 2404.05622), ground-truth sources (NZBs, takedown STAT). C real-world posting throughput (articles/s per connection, typical connection counts, upload speeds) to set session-gap thresholds and BOCPD priors.
- Peer (Opus payload wave 9, 10:55Z): encrypted-repost size key ~23-27 bits except fixed-cap disc images (44% share a size). Pure-Rust CRC32 GF(2) toolkit at opus-tools/w9b_crc32gf2/. Payload tool tells at opus-tools/w9c_tool_tells.md. Cross-reference only.
- Peer (Haiku 10-min posting thread wave 9, 11:01Z): Postie PAR2 real-name leak lives in CreateSet/setInputNames (internal/par2/par2.go:223-259, existing.go:233-243); FileDesc = real relative path under every policy. ngPost --compress --gen_name randomizes before RAR; --gen_par2 alone leaks. INN nnrpd adds Injection-Date only when Message-ID or Date is missing (post.c ~400-405), so Nyuu/Postie posts usually lack it; Injection-Info logging-data = nnrpd PID (per-connection key), posting-host = client host in cleartext.
- Peer (Opus posting thread wave 9, 11:03Z): on 202 public NZBs Nyuu files post strictly in series; end-to-start gap (decoded ms) median 0.12 s, p90 0.67 s, 98% < 2 s; intra-file median 0.018 s. Whole-second NZB dates give only 38% < 2 s. Gap-only relinking precision .93 at 2 concurrent uploads, .60 at 10 simultaneous, ~.98 for 10 spread over an hour; a size-like feature adds .15-.25. Highwinds: S constant per NZB, N rises in 861/862 steps; +2/+4 steps = other traffic on the same front end. Scripts: research/posting-behavior/sim/w8a, sim/w9b.
- Wave 10 adjustment: A = Astraweb 5th field + whether Highwinds S/K changes between NZBs from one uploader (per connection, per front end, per day); B = validation, reuse sim/w8a + sim/w9b and add Splink/2404.05622 metrics design; C = throughput/gap norms for ngPost and Postie (Nyuu already measured by the Opus thread).

## Wave 10 (launched 11:24Z): A Astraweb 5th field + Highwinds K scope + other provider ID families; B stitching validation (Splink, 2404.05622, BOCPD vs threshold, ground truth loop); C ngPost/Postie throughput and gap thresholds

### W10C result (throughput, gaps)
- ngPost: files strictly sequential, articles of one file spread across all connections (PostingJob.cpp:640-692, Poster.cpp:84-101); no throttle; 716800-byte articles; socket timeout 30 s then reconnect. No Date header.
- Postie: MaxConcurrentUploads 1 (config.go:695/1176) but pipeline depth 2 files (poster.go:618-621), so file boundaries OVERLAP. Optional token-bucket throttle (default off). Retries 3 x 5 s; post-check repost 10 s to 5 min backoff.
- User reports: ~1.2-1.4 MB/s per connection (Nyuu 8-45 conns: 9-55 MB/s); ngPost 60 conns ~10 MB/s on one 1 Gbit line, 20 conns ~30 MB/s on another.
- Computed: 750 kB articles at 1 Gbit = ~163/s (6 ms gap); 40 conns real-world ~68/s (15 ms), matching Nyuu's measured 18 ms intra-file median.
- Thresholds: within-file split > 2 s (hard 5 s) ngPost, > 5 s Postie; between uploads >= 90 s ngPost, >= 300 s Postie (post-check backoff). File gap is near zero or negative, so never cut sessions on file gaps.

### W10B result (validation)
- arXiv 2404.05622 = Binette et al., entity-centric ER evaluation: cluster precision/recall estimators from PPS-sampled clusters (p_c = |c|/N), ratio estimator with bias correction and SD. Python package er-evaluation implements it.
- Splink graph metrics: cluster size, density (edges/max edges), is_bridge; no thresholds given.
- BOCPD: constant hazard 1/lambda (start ~100), Normal-Inverse-Gamma priors on standardized data; augurs-changepoint / changepoint crate take &[f64] values: feed log inter-arrival gaps, not timestamps. HDBSCAN on 1-D time fails at K >= 10 concurrent uploads.
- Existing gap-only numbers (peer sim/w9b out_t2c): K=5 over 10 min P .93 R .89; K=10 simultaneous P .63 R .57; K=10 over 1 h P .98 R .91. Q4 re-run not done.
- 9-step validation procedure recorded (ground truth = NZB grouping AND PAR2 file-ID agreement; dedupe NZBs by Message-ID set; split by ID family and time source; Binette estimator weekly; STAT-430 takedowns as censoring signal only).

### W10A result (server ID families)
- 13+ real Astraweb IDs from GitHub fixtures (frosthoar/hoarder, nzbfast, sabctools, leonarduk/unison). Field 4 takes 2 values (c3e8da3, b1db1813) = 2 hosts. Counter increments within a second (56ba4d4c$0, 56ba4d4c$1). Field 5 per-article, matches no CRC/shash of the ID's own fields: probably a hash of something outside the ID (client IP, account or body). shash is tail-dominated (astraweb.com, nntp.astraweb.com also give 0c3e8da3): host-family fingerprint, not exact hostname.
- Highwinds T1.N$K.R@fxNN.{am4,iad}: ~17 real IDs. N per front end, steps +1/+2/+4. K constant across all segments of one NZB, differs between NZBs on the same day (not per day). R per segment, random-looking. T1 5 chars base64-like, first char advances with N; not decoded.
- Same T1.N$K.R family also on news.usenetserver.com, news.giganews.com, news.easynews.com (older); Diablo 4-field form on news.usenetserver.com and news.newshosting.com (2002-2009). No Eweka/Frugal/XS Usenet samples.
- Peers 11:31Z: Opus yEnc wave 9 (old Nyuu Node 4 MWC1616 brute force; size-vector twin key; Rust spec research/yenc-header-forensics/opus/w9b-atlas-rust-spec.md). Opus payload thread finished: https://claude.ai/artifact/Kokmceq4dVK46C4oDHaKgD.

## FINAL REPORT (11:35Z): https://claude.ai/artifact/SdC4pYwLdtnPpd1KjXBhDd (Claude Doc "Posting behavior linking signals")
