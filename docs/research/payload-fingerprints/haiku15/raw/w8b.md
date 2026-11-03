# W8B NNTP fetch efficiency (Haiku)
- RFC 3977 §3.5 pipelining allowed, in-order. RFC 4644 STREAMING is feed-side only. RFC 8054 COMPRESS DEFLATE: not pipelined, after STARTTLS+AUTHINFO, opt-in under TLS (CRIME). XZVER/XZHDR (Astraweb): zlib then yEnc; others unverified. XFEATURE COMPRESS GZIP: no primary source.
- Rust: nntp-rs 0.3 (tokio, rustls 563, COMPRESS DEFLATE + XFEATURE GZIP, bb8 pool, fetch_xover); nzb-nntp 0.2.26 (pipelines ARTICLE/STAT, XFEATURE GZIP, failover); nntp 0.0.5 dead.
- Strategy: range OVER in big batches, pipelined, 4-8 conns, CAPABILITIES first, test compression per provider. No measured throughput found.
