# W5C body mutation across providers (Haiku, ~08:58Z)
- RFC 3977: dot-stuffing undone once at line start; no body line-length limit; bare CR/LF forbidden; nothing mandates byte preservation in transit. RFC 5536: relays rewrite Path/Xref/Injection-* headers only.
- INN maxartsize rejects (not truncates) oversized articles -> "missing on this server", not different bytes. lists.isc.org inn-workers 2005-June/014264
- Real body corruption precedent: Thunderbird NNTP-JS dot-stuffing bug (bugzilla 1787955).
- yencoding 0.2.0 decoder behaviour (double-dot strip, discards CR/LF/NUL, split escape carry, CRC gating). docs.rs yencoding decode.rs
- No source found that any backbone alters bodies, or that X-Received-Body-CRC / X-Ufhash cover the body. UNVERIFIED -> measure: fetch 20-50 Message-IDs from 2-3 providers (Atlas has Eweka + others) and diff raw bodies.
Normalisation checklist: dot-unstuff once; CRLF only, flag bare CR/LF; strict =ybegin/=ypart/=yend parse; decode; key only CRC-verified segments; key on decoded CRC+size; ignore relay headers; missing != different.
