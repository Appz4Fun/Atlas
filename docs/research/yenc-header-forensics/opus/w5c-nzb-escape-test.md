# Wave 5C: real-NZB test of the escape-count twin key (Opus subagent, partial)
Dataset: 104 NZBs / 2814 files / 69,181 segments, mostly github.com/sanderjo/NZBs (Nyuu, 716800/128, alt.binaries.test) + hellanzb, nzbget, Prowlarr fixtures. zurg nzbbench corpus not reachable.
1. MEASURED: Nyuu's NZB <segment bytes> = yEnc body + =y lines, NO NNTP headers. Zero-escape files: bytes = 716800 + 2*5600 + len(=y lines) + 3, residual constant 3 (part 1 +3..5). So escapes per segment exactly recoverable: esc = bytes - P - 2*ceil(P/L) - hdrlen - 3. SABnzbd/NZBGet/indexer NZBs (which use :bytes with headers) not tested.
2. MEASURED: random content full segments mean 739,550-739,710, per-file sd 57-148 (~100), model 739,504+~140, sd 105. Mean escapes 11,384 vs 11,325 predicted. Low-entropy content (zero-filled RARs) ~0 escapes -> key useless there.
3. Same post in two NZBs: identical sequences (metadata stable; same msgids, so not a repost test). Same name+size different content (PAR2 volumes of different sets): unrelated sequences as expected.
4. False match (62,759 unrelated random-content file pairs): per-segment equality 0.22% (theory ~0.28%); k segments ~0.0028^k, <1e-10 for k>=4. 39 full-sequence equal pairs = zero-escape files or shared msgids.
Verdict: partly supported. Discriminating power confirmed; repost identity untested (no independent repost in corpus); needs entropy gate (mean escapes > ~5000/part).
Leads: find real reposts (toverdoos blabla.bin set, needs refit for preset); NZB bytes convention as NZB-writer fingerprint (Nyuu exact, residual 3); escapes/part as stored-vs-compressed classifier.
Scripts: nzb-escape-test/.
