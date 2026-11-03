# Wave 08-A: bulk per-article headers over NNTP without BODY

Scope: which headers can be pulled in bulk over a range without BODY, and at what cost.
Budget: about 20 tool calls, about 10 minutes. Per-provider claims are thin. Most of them are forum posts and marked as such.

## 1. Protocol basics

- LIST OVERVIEW.FMT returns the overview field list, with the 7 defaults first (subject, from, date, message-id, references, :bytes, :lines). Extra fields follow, and each one is either a header name (e.g. `Xref:full`) or a `:` metadata item. (verified from source: CPython nntplib `_DEFAULT_OVERVIEW_FMT`, `_parse_overview_fmt`, `_parse_overview`. https://raw.githubusercontent.com/python/cpython/3.12/Lib/nntplib.py, also main)
  - The parser keeps the suffix after the colon, e.g. `Xref:full`, and ignores it. The source comment says "Should we do something with the suffix?". So the `:full` suffix is not documented as meaningful by the code. (verified from source, same URL)
  - Non-default header values come back as `Name: value` inside each overview line, and the parser strips that prefix. (verified from source)
- XOVER/OVER gives the overview lines. nntplib's `over()` falls back from OVER to XOVER when OVER is not advertised. (verified from source)
- XHDR (and HDR, which RFC 3977 says is the successor) can ask for one named header across a range or a list of message ids. CPython nntplib implements only XHDR. (verified from source)
- The RFC 3977 section 8 text (overview, `:bytes`/`:lines`, HDR, LIST HEADERS) could not be read through the fetch tool. The fetch tool stopped at section 6.2.x. The rfc-editor copy would need a full download, which the proxy blocked. Not verified: the exact HDR syntax and the `:full` rules from the RFC itself. (docs, incomplete)
- COMPRESS DEFLATE (RFC 8054): `COMPRESS DEFLATE` returns 206 and turns on compression right after that CRLF. Response codes: 403 (cannot activate), 501 (bad name), 502 (already active), 503 (algorithm not supported). The command itself MUST NOT be pipelined, and the client must wait for its reply. Everything after it is compressed, including multiline data. (docs, verified from the RFC 8054 text via WebFetch: https://www.rfc-editor.org/rfc/rfc8054.txt)
- XZVER / XZHDR: informal Astraweb proposal. Returns 224/221 with compressed XOVER/XHDR data. The wiki page says the payload is deflate-compressed then yEnc-wrapped, and the yEnc `size` field is a placeholder (-1). The page does not list the fields. (docs, informal; https://cal1.lr.ggtyler.dev/r/UsenetTalk/wiki/nntp/xzver, mirror of r/UsenetTalk wiki)
- XFEATURE COMPRESS GZIP: a headers-only legacy compression used by Newsbin and others. Some servers reject it with 500. (forum claim; https://forums.newsbin.com/viewtopic.php?p=222309)

## 2. Per provider (what people report)

- Astraweb (ssl-us.astraweb.com): supports XZVER and XFEATURE COMPRESS GZIP. (forum claim; https://forums.newsbin.com/viewtopic.php?p=222309)
- news.usenet.farm and news.cheapnews.eu: XZVER only, no XFEATURE COMPRESS GZIP. The usenet.farm poster says header bandwidth counts toward block usage. (forum claim; same URL)
- eunews.blocknews.net: XZVER supported, but 224 responses are multi-line rather than single-line. (forum claim; same URL)
- usenet.premiumize.me: no compression of either kind. (forum claim; same URL)
- Eweka/Omicron, Highwinds/Newshosting/UsenetServer, UsenetExpress, Newsdemon/Abavia, Giganews: no verified data found this round. Their LIST OVERVIEW.FMT output, HDR support for Injection-Info / Injection-Date / X-Received-Bytes / NNTP-Posting-Host / Path / Xref / Organization, `:lines`/`:bytes` fill, and XZVER/COMPRESS support are all unknown. (guess until someone pastes a CAPABILITIES dump or an overview.fmt listing)
- Other server behavior to expect: many providers return a 503 or an empty overview for an unlisted header. Those are not in the overview unless the server admin adds them. Standard advice from indexer communities is to probe with `LIST OVERVIEW.FMT` at connect time and cache the result. (guess; no source fetched)

## 3. Rust NNTP crates (checked from crate source on static.crates.io)

Downloads are from the crates.io API. GitHub stars are not visible because api.github.com is blocked.

- nntp-rs 0.3.0 (453 total downloads, 58 recent). Has COMPRESS DEFLATE with a fallback to XFEATURE COMPRESS GZIP (headers-only). Has pipelining tests and an RFC 8054 test file. Did not check whether it has HDR/XHDR/XZVER. (verified from crate source: src/client/compression.rs, tests/rfc8054/compression.rs, tests/pipelining_test.rs)
- nzb-nntp 0.2.26 (2,314 total, 862 recent). Parses CAPABILITIES for HDR, OVER, OVER MSGID, LIST keywords, COMPRESS / XFEATURE-COMPRESS. Pipelining is in config. Uses XOVER in connection.rs. No XZVER. (verified from crate source: src/capabilities.rs, src/connection.rs)
- rek2_nntp 1.0.6 (12,210 total, 100 recent). XOVER only. (verified from crate source: src/xover.rs)
- nntp 0.0.5 (9,822 total). Nothing for overview, HDR, or compression found in the crate. (verified from crate source: no matches)
- Not checked: pipelining in rek2_nntp and nntp 0.0.5.

## 4. Linking value of the headers (guess unless marked)

- Injection-Info (RFC 5536 posting-host / injection-date / posting-account) and Path: possible same-uploader-session signal. The posting-host hash can match across parts. (guess; header semantics from RFC 5536, not fetched this round)
- Xref: crossposts, which help group related posts across newsgroups. (guess)
- X-Received-Bytes: a provider-added header, not standard, varies by provider. Probably useless for linking across providers. (guess)
- NNTP-Posting-Host: often stripped or hidden by providers. Check on a sample before relying on it. (guess)

## 5. Notes on code (the indexer side)

- sabnzbd and nzbget clones contain no XOVER, OVER, HDR, XHDR or COMPRESS code that matches these searches. So these clients do not depend on bulk overview. (verified from absence in the clones at /tmp/w8a/sabnzbd and /tmp/w8a/nzbget)
- nntmux and other repos were not cloned: GitHub clone for nntmux failed on auth.

## Top 3 leads for the indexer

1. Probe LIST OVERVIEW.FMT on connect and cache per server. Only trust `Xref`, `Injection-Info`, or `Path` as overview fields if the server lists them. The nntplib parse rules are a ready reference. (verified source)
2. Send COMPRESS DEFLATE once at connection start, before any pipelined reads. Its own command is not pipelined. Fall back to XFEATURE COMPRESS GZIP or XZVER only per provider, as nntp-rs does. Expect Astraweb-style providers to support XZVER and some not. (docs and forum claims)
3. Test HDR (not XHDR) per provider for Injection-Info, Path, Xref, and X-Received-Bytes on a 1,000-article range without BODY. Measure bytes and time. This is the only route to arbitrary headers without fetching bodies. (guess until tested)
