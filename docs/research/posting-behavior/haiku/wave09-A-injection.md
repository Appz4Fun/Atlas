# Wave 09-A: server-side injection headers and cross-article linking

Scope: what news servers add or strip on binary article injection, and which values survive uploader randomization of From, Subject, Message-ID and Date. Budget-limited pass (about 25 tool calls). Confidence tags: verified in source / docs / real dump / forum claim / guess.

## 1. INN nnrpd (source read: InterNetNews/inn, clone --depth 1)

- Injection-Info is built in nnrpd/post.c:569-638. Format: `<pathidentity>; posting-account="..."; posting-host="..."; logging-data="<pid>"; mail-complaints-to="..."`. [verified in source: https://github.com/InterNetNews/inn/blob/main/nnrpd/post.c#L569-L638]
- posting-host is the raw client IP and/or reverse-DNS name (`Client.ip`, `Client.host`, `Client.host:Client.ip`). It is NOT hashed in INN. [verified in source: nnrpd/post.c:592-607]
- posting-host is emitted only when `addinjectionpostinghost` is set, which defaults to true. [docs: doc/pod/inn.conf.pod ~L1217-1225]
- posting-account is emitted only when `addinjectionpostingaccount` is set, which defaults to false. The value is `PERMuser`, the authenticated username. The docs say there is no built-in obfuscation. [verified in source post.c:586-590; docs inn.conf.pod ~L1209-1215]
- logging-data is `getpid()` of the nnrpd child process (post.c:610-630). The nnrpd child serves one NNTP connection, so this is a per-session constant. PIDs recycle, so it is not globally unique. [verified in source]
- mail-complaints-to comes from the `complaints` config, else `NEWSMASTER@fromhost`. It is per-server, not per-user. [verified in source post.c:~611-625]
- Injection-Date is set from the article Date or current time (post.c:395-411). It is a timestamp, not a session key. [verified in source]
- Path: INN prepends `pathhost` (inn.conf.pod ~L131). When `addinjectionpostinghost` is true, the Path gets a `.POSTED.<Client.host>!` prefix (post.c:515-520), or `.POSTED!` when the option is off (post.c:525-528). So the client host (rDNS or IP) appears in cleartext in Path as well as in Injection-Info. [verified in source]
- NNTP-Posting-Host, X-Trace, X-Complaints-To, NNTP-Posting-Date, Date-Received, Posting-Version and Relay-Version are listed in the post.c header table as HTobs (obsolete). I did NOT verify whether INN deletes them on injection or passes them through. [verified in source for the table only]
- Xref is listed as HTstd in the same table. It is added by innd at feed time, not by nnrpd. Not verified in this pass. [guess for the mechanism]
- RFC 5536 definitions (rfc-editor.org/rfc/rfc5536.txt):
  - Injection-Info section 3.2.8: posting-host is the FQDN and/or IP of the host the server received the article from. posting-account identifies the source "in a notation only the server administrator can interpret. Messages from the same source should share a value." logging-data "typically a session number or other non-persistent means of identifying a posting account." [docs]
  - The RFC says older NNTP-Posting-Host, X-Trace and X-Complaints-To are obsolete once Injection-Info exists. [docs]
  - Injection-Date MUST be inserted on every injection (section 3.2.7). [docs]
  - Path: each agent prepends at least one path-identity (section 3.1.5). [docs]
  - Section 3.1.3 and 5: a generator "SHOULD ensure that [message IDs] are unpredictable." [docs]

## 2. Real header dumps

- hashcat.net forum post 31614 (fetched 2026-11-02): a pasted Eternal September article. Injection-Info shows `mx02.eternal-september.org; posting-host="3a7acd6b6bbfe9e4e33e3a357762cf34"; logging-data="10878"; mail-complaints-to="abuse@eternal-september.org"; posting-account="U2FsdGVkX19..."`. [real dump, forum claim on exact content]
  - The posting-host is a 32-hex value, so it is hashed. The posting-account is an OpenSSL-style "Salted__" base64 blob, so it is encrypted with a salt. [real dump, but only for Eternal September]
  - Eternal September is not INN default behaviour. The hashing happens at that provider. Do not generalize.
- The same post shows individual.net X-Trace: `X-Trace: individual.net <base64-ish token>`. [real dump, forum claim]
- No full binary article HEAD dump was found in this pass. The search did not surface Nyuu, ngPost, postie or SABnzbd GitHub issues with pasted HEAD output. Missing: Path entries from Highwinds, Omicron, Eweka, UsenetExpress and Newsdemon; X-Received-Bytes; X-Received-Body-CRC; Organization.
- X-Received-Bytes and X-Received-Body-CRC: a search for the exact header names returned nothing relevant. Not verified. [guess]

## 3. Server Message-ID families

- Searches for Highwinds `T1.N$K.R@fxNN`, `[A-Za-z]{32}@UFK` and Astraweb `$` IDs returned no sources. Treat all three as unverified. [guess]
- RFC 5536 and the usefor drafts say only that IDs should be unpredictable and use a domain name as id-right. [docs]
- Sender-side: Nyuu randomizes Message-ID unless `keepMessageId` is set. lib/article.js:240-256 (`randomizeMessageID`) rebuilds the ID from the template and pads or substitutes `-` to keep the original length. lib/nntp.js:828 calls it. [verified in source, github.com/animetosho/Nyuu]
  - Nyuu's randomized ID keeps the original length, so length is a weak fingerprint across posts. It does not carry a per-session value. [verified in source, inferred effect]
- Nyuu also sends `XREPLIC` with a random integer (nntp.js:832). This is a client-to-server command, not an article header. [verified in source]

## 4. Per-header linking analysis

| Header | Who sets it | Per-session constant? | Per-account constant? | Hashed? | Linking value |
|---|---|---|---|---|---|
| Path `.POSTED.<client host>!` | INN nnrpd, when addinjectionpostinghost is on (default) | yes (same connection) | yes (same IP/rDNS) | no in INN | High. Cleartext client IP or rDNS. Survives across groups and obfuscated posts unless the reader's server strips it. |
| Path first hop (pathhost) | Injecting server | no | no | n/a | Low. Same for every post from that server. |
| Injection-Info posting-host | INN, when addinjectionpostinghost is on | yes | yes | no in INN, yes at Eternal September | High on INN. Medium on hashed providers, still stable per source. |
| Injection-Info logging-data (PID) | INN nnrpd | yes, one per connection | no | n/a | Medium to high. Groups articles from one connection. PIDs recycle, so collisions are possible. |
| Injection-Info posting-account | INN, when addinjectionpostingaccount is on (default off) | yes | yes | no in INN, yes at Eternal September | Very high where present. Likely rare. |
| Injection-Info mail-complaints-to | Config | no | no | n/a | None for linking. |
| Injection-Date | INN, from article Date or now | no | no | n/a | Low. Timing only. |
| X-Trace, NNTP-Posting-Host | Varies (individual.net X-Trace seen) | probably yes if present | yes | varies | Unknown without dumps. Obsolete per RFC. |
| X-Received-Bytes, X-Received-Body-CRC | Unknown provider | unknown | unknown | unknown | Unverified. |

## Top 3 server-side linking signals

1. Path `.POSTED.<client host>!` and Injection-Info `posting-host`, when the server is INN with defaults. Both carry the same client IP or rDNS for every article from one connection, in cleartext. Confidence: verified in source for INN. Real-world behaviour depends on whether the target server is INN with defaults.
2. Injection-Info `logging-data` (nnrpd PID). It is constant within one NNTP session, so it links articles posted in the same connection regardless of From, Subject, Message-ID or Date. Confidence: verified in source for INN. PID recycling limits how far the linkage can be trusted.
3. Injection-Info `posting-account`, if present. It is an authenticated per-account value, and hashed or salted variants (Eternal September) remain stable per source. Confidence: docs for INN, real dump for Eternal September. Rare unless the provider enables it.

## Gaps

- No real dump of a binary article from a commercial provider (Highwinds, Omicron, Eweka, UsenetExpress, Newsdemon). Their behaviour is unverified.
- Message-ID families from those providers are not verified.
- Whether a provider strips Path or Injection-Info before serving to readers is not verified.
