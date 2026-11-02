# Wave 10 merged (11:31-11:36Z): provider headers, fingerprint gaps, ground truth

## A. Commercial provider headers
- No real HEAD dump of a binary article from Highwinds/Omicron, UsenetExpress, Abavia/Newsdemon, Giganews or usenet.farm was found anywhere public. Every server-header claim for these providers stays unverified.
- Nyuu source (lib/nntp.js ~820-875): if the server's POST reply returns a different Message-ID, Nyuu replaces its own with it. So servers can rewrite IDs; key on the ID from HEAD/overview, never the client's.
- Ownership (community wiki): Newshosting, UsenetServer, Eweka, Astraweb, Tweaknews are Highwinds-family; Newsdemon left Highwinds in 2020; UsenetExpress and usenet.farm are independent. Expect two or three header families, not seven.
- Store from HEAD per article: Message-ID (server value), Path, Newsgroups, Xref, Injection-Info, X-Trace, NNTP-Posting-Host, Date, Injection-Date, plus a raw bag of all other X-* headers with distinct-value counts per provider.

## B. Fingerprint gaps (source-checked)
- NewsUP: 768000 B, line=128, header order From, Newsgroups, Subject, [Message-ID]. Without --generate-ids it sends NO Message-ID (server assigns). Obfuscation keeps the REAL yEnc name= (Article.pm:114, author TODO). It builds a "rename" PAR2 from real names before renaming and uploads it, so the real names leak through that PAR2. Its obfuscation picks Message-IDs that MIMIC other tools (nyuu `[A-Za-z0-9]{12}-\d+@nyuu`, JBinUp `{32}@JBinUp.local`, gopoststuff with a typo `@gpoststuff`); these mimics differ from the real formats, so a near-miss shape points at NewsUP.
- nxg-upper: 768000 B (multiple of 64), line 128, deterministic SHA256 IDs from the NxG header; header map order random. With RAR + ObfuscateRar (default on) the RAR and PAR2 base is a random 21-char string, so FileDesc names are obfuscated; ObfuscateYenc gives md5(name+part).
- GoPostStuff: 768000 B (sample config), line 128, `<UnixNano$gps@gopoststuff>`, order From, Newsgroups, Message-ID, X-Newsposter, Subject, no Date.
- sanguinews: 768000, line 128, no Message-ID, X-Newsposter, Date present.
- JBinUp: no source found.
- Nyuu size, resolved: config.js:83 sets articleSize 716800 for the CLI; lib/uploadmgr.js:140 has a 768000 fallback used only when the library is called without a config. Real Nyuu posts are 716800 unless the user overrides it.

## C. Ground truth
- Anime Tosho stopped ingesting about 2026-05-09 and its feed server expired in October 2026, but the data dump (at-dbdump, gzipped NZBs keyed by torrent ID, 1 TB at-data torrent) is offered with "use in any way you like". Gives silver labels for twins and alts, not for split posts.
- NZBIndex robots.txt: 1 request per hour, no AI crawlers; not usable for bulk. Spotnet NZBs in alt.binaries.ftd remain the best free label source for split posts.
- Metrics: pairwise P/R, B-cubed, purity, reported separately for split posts, twins and alts. Split dev/test by release and time. Hand-label about 300 pairs per split, double-label 10%.
