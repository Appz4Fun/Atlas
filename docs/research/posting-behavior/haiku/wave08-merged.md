# Wave 8 merged (10:26-10:31Z): bulk headers, prevalence, evasion

## A. Bulk header retrieval without BODY
- COMPRESS DEFLATE (RFC 8054): send once right after connect. The command itself must not be pipelined; everything after the 206 reply is compressed. XZVER/XZHDR is an informal Astraweb extension (deflate then yEnc-wrapped); usenet.farm, cheapnews and blocknews also support it per forum posts. XFEATURE COMPRESS GZIP is a headers-only legacy variant.
- No verified LIST OVERVIEW.FMT, HDR or CAPABILITIES data was found for Eweka/Omicron, Highwinds, UsenetExpress, Newsdemon or Giganews. This is a gap that only a live probe from Atlas can close.
- Rust crates are all small: nzb-nntp (2.3k downloads) parses CAPABILITIES (HDR, OVER, COMPRESS) and has pipelining; nntp-rs (453 downloads) has COMPRESS DEFLATE with GZIP fallback. Under the dependency rule, write our own.
- Probe plan: LIST OVERVIEW.FMT and CAPABILITIES at connect (cache per server), then HDR over 1,000 articles for Injection-Info, Injection-Date, Path, Xref, X-Received-Bytes; measure bytes and time.

## B. Prevalence
- No public measurement exists of how many posts are obfuscated, group-per-file, or NZB-less. Only qualitative forum claims (Newsbin admins 2017-2022, r/usenet 2024: "the meat and potatoes are within the indexers").
- Defaults matter: ngPost's default GROUP_POLICY is ALL (each_file is opt-in), Postie's default is each_file. So group-per-file splitting is mostly Postie or deliberately configured ngPost.
- Action: measure it ourselves from Atlas's own overview rows (share of hash-like subjects, random posters, per-file group rotation per group per day).

## C. Evasion features and signal survival
- No shipped uploader randomizes article size, yEnc line length, posting order, adds jitter or junk files. The Nyuu maintainer rejected article-length variation (issue #121: "Modifications which alter an article's length will not work in Nyuu").
- Postie article size settled: default 750000, preset 768000 (config.go:574-575, 1110).
- Postie full: random Subject, yEnc name, From, Date (now-6h) per article. Kills the Date vs Injection-Date signal and weakens Date-based overlap; server arrival time (Injection-Date / article number order) still works.
- Postie PAR2 policy renames PAR2 files but builds PAR2 packets with the REAL basename (par2.go:107-113), so the PAR2 FileDesc name leaks the real filename. Big win for naming.
- Password RAR with -hp kills the RAR5 chain beyond the volume header (unconfirmed which tools use -hp).
- Most robust signals: the yEnc part/total/size/begin/end chain; the PAR2 FileDesc bridge (content-derived); article size bucket + line=128 as tool fingerprint.
