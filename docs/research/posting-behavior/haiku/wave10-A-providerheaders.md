# Wave 10-A: provider-added headers on binary articles (2018-2026)

Status: NO real full HEAD dumps from commercial providers were found. Every search for the target strings (X-Received-Body-CRC, X-Received-Bytes, Injection-Info news.eweka.nl, X-Complaints-To abuse@newshosting, news.usenetexpress.com, highwinds-media.com in Path/Message-ID) returned only RFCs, INN mailing-list threads, or unrelated pages. Nyuu issue search was blocked by GitHub robots. The header-level claims below are therefore docs, source-code, or forum claims, not dumps. Tags: real dump / docs / forum claim / source (read from a cloned repo) / guess.

## 1. What the client repos show (source, verified in clones)

- Nyuu (https://github.com/animetosho/Nyuu, cloned depth 1): `lib/nntp.js` ~L820-875. Nyuu generates its own Message-ID (`post.randomizeMessageID()` unless `--keep-message-id`). On a POST response, if the server's 2xx info line matches `RE_POST`, Nyuu replaces its local ID with the server-returned one and warns "Server returned a different Message-ID ... replacing our ID with received ID". Tag: source. Implication: the server can rewrite the Message-ID, so Atlas should key on the value in the response/HEAD, not the client-sent one.
- Nyuu `lib/article.js` L82-86 and L489-497: a client-supplied Message-ID is accepted and must match `<...>`. Default IDs are random mixed-case letters (L204-221). Tag: source.
- ngPost (https://github.com/mbruel/ngPost): sets no provider-specific headers; docs and config use `news.newshosting.com` as the example host (README.md L132). Tag: docs.
- sabnzbd (https://github.com/sabnzbd/sabnzbd, `sabnzbd/get_addrinfo.py` L36-47): IPv6 host mapping for news.eweka.nl, news.astraweb.com, news.newshosting.com, news.usenetserver.com, etc. to `news6.<host>`. This is a connection-level detail, not a header. Tag: source.
- postie and sabnzbd repos contain no fixture with a real provider article header (grep for X-Received, Injection-Info, X-Trace, Complaints-To, highwinds, usenetexpress, usenet.farm found nothing relevant).

## 2. Header-format references (docs)

- Harley Hahn Usenet tutorial (https://harley.com/usenet/usenet-tutorial/the-format-of-a-usenet-article.html): generic article headers only. Tag: docs.
- Big-8 wiki (https://www.big-8.org/wiki/Headers), fetched: example `Path: border1.nntp.dca.giganews.com!nntp.giganews.com!...!not-for-mail`, `X-Trace: reader1.panix.com 1260746750 22049 166.84.1.6 (...)`, `NNTP-Posting-Host: panix6.panix.com`, `Xref: number.nntp.dca.giganews.com comp.archives.msdos.announce:10840 ...`, `Message-ID: <hg3q9m$2p2$1@news.albasani.net>`. These are 2009-era examples from a non-binary-indexer context. The page does not say which element sets each header. Tag: docs (old examples, not binary posts from the target providers).
- Path semantics: each relay appends its name followed by `!`; the first hop is the injecting/posting server. Tag: docs (general Usenet convention, consistent with the above examples).

## 3. Provider ownership (forum/wiki claim)

Source: r/UsenetTalk "History of Usenet Providers" wiki, mirror https://axebps-redlib.hf.space/r/UsenetTalk/wiki/providers/history. Tag: forum claim (community wiki, not verified).
- Highwinds owns or owned Newshosting (2005), UsenetServer/UNS (2006), Easynews (2006), Eweka (2007), Tweaknews and Base IP/Euroaccess (2014), Astraweb (taken over 2018, after its IP resolved to Highwinds in Dec 2017).
- Omicron (Omicron Media/Omicron Networks, renamed 2017) is described as Highwinds-related. Treat Omicron as the same backend family, not a separate infrastructure, until proven otherwise.
- Eweka: Highwinds subsidiary since 2007 (not a separate Omicron provider). Newshosting: Highwinds since 2005.
- UsenetExpress: US operations started 2017, no ownership change listed.
- Abavia / NewsDemon: backbone moved to Abavia 2016; NewsDemon's Highwinds/Omicron contract was not renewed in 2020 and it planned its own backbone. So Newsdemon headers from 2020 onward may differ from Highwinds-stack headers.
- Giganews: acquired Supernews in 2008, Supernews infrastructure subsumed. No header claim.
- usenet.farm: Dutch Tier 1 since 2015, no ownership change listed. No header claim found.
- Blocknews: reseller of Highwinds from 2007, moved to Readnews 2009. Expect Highwinds-like headers only if the backend is still Highwinds (unverified).

## 4. Forum threads checked (no header dumps)

- SABnzbd forum "Free Newsservers" p.6 (https://forums.sabnzbd.org/viewtopic.php?p=8804): 2008 IPv6 and free-server chatter. No headers.
- Newsbin forum p=230671 (https://forum.newsbin.com/viewtopic.php?p=230671): Newshosting to Eweka switch; Eweka posting uses a separate host `post.eweka.nl` port 119 (forum claim). No headers.
- Google Groups van.general thread (https://groups.google.com/g/van.general/c/2wnTJGnGq8M/m/3zYxzFemBwAJ): rendered view only, no raw headers.

## 5. Per-provider header claims

| Provider | Headers seen | Path first hop | Per-account/session value | Message-ID format | Tag |
|---|---|---|---|---|---|
| Highwinds (Newshosting, UsenetServer, Eweka, Astraweb, Omicron) | none verified | not verified; reported as `...highwinds-media.com` hostnames (community mention only) | unknown | unknown; reported `@fxNN.<pop>.highwinds-media.com` (unverified) | guess |
| Newscene / Highwinds feeders | Message-ID passthrough (wiki) | feeder hop | none | `newscene.com` IDs pass through Highwinds feeders | forum claim |
| UsenetExpress, Abavia/NewsDemon, Giganews, Astraweb, Blocknews, usenet.farm | none found | unknown | unknown | `@news.usenet.farm` reported in the brief only, not verified | guess |

No provider's X-Received-Body-CRC, X-Received-Bytes, Injection-Info, X-Complaints-To, or X-Trace values were observed in any source. Do not treat any header name in this table as confirmed for these providers.

## 6. Suggested HEAD fields for Atlas to store per article (to test these providers)

Store these only from a real HEAD of an article the indexer already has; the cardinality estimates are guesses.

1. `Message-ID` (string, one per article). Why: the server may rewrite it (Nyuu source shows this); key on the server value. Cardinality: unique per article.
2. `Path` (string). Why: first hop identifies the injecting server; the full list identifies relay chain. Cardinality: few distinct first hops per provider (guess: 1-5); full string unique-ish.
3. `Newsgroups` and `Xref` (strings). Why: Xref carries server-assigned article numbers per group, useful for matching the same post across servers of the same provider. Cardinality: one per group per article.
4. `Injection-Info` and `X-Trace` (strings, if present). Why: may carry a host and a session/posting token. Cardinality: if per-session, high (unique per post); if per-host, low. Check whether the token varies between two posts from the same session.
5. `NNTP-Posting-Host` (string, if present). Why: reveals the posting client IP or hash. Cardinality: low to medium.
6. `Date` (timestamp). Why: server injection time vs. client time (`Date` vs. `X-Received` timestamp). Cardinality: unique per article.
7. `Lines` and `Bytes` (or `:bytes` metadata). Why: provider may report size, useful against the NZB segment size. Cardinality: unique per article.
8. Any `X-*` header not in the list above, stored as a raw key/value bag with a count of distinct values seen per provider. Why: X-Received-Body-CRC, X-Received-Bytes, X-Complaints-To are the names in the brief; their presence is unverified. Cardinality: collect first, then decide.

## 7. Open items (what would confirm or refute these)

- A real HEAD dump from a Highwinds-stack provider (Newshosting or Eweka) on a binary post from 2024-2026.
- A real HEAD dump from Newsdemon/Abavia and from usenet.farm, to compare `Path` first hops.
- A Nyuu GitHub issue containing a raw header block (blocked by robots in this session; retry via gh CLI or a logged-in browser).
