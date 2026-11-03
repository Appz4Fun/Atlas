# Wave 9A: Nyuu's bundled Node versions, and the base64url poster

## Part 1: which Node the Nyuu binaries bundle

Sources: raw `nexe/build.js` at each tag (raw.githubusercontent.com/animetosho/Nyuu/<tag>/nexe/build.js), the npm registry times (registry.npmjs.org/nyuu), and the GitHub releases page notes.

| Release | npm publish date | Bundled Node (nexe build.js) | Math.random |
|---|---|---|---|
| v0.3.1 | 2017-02-01 | 4.7.3 | V8 4.5, **MWC1616** |
| v0.3.6 | 2018-01-16 | 4.8.3 | MWC1616 |
| v0.3.8 | 2018-05-30 | **4.9.1** | MWC1616 |
| v0.4.0 | 2021-01-02 | 12.20.0 (release note: "Static Windows/Linux x86-64 builds using NodeJS 12.20.0") | xorshift128+, new ToDouble (state0>>12) |
| v0.4.1 | 2022-05-31 | 12.22.12 (release note) | new |
| v0.4.2 | 2024-01-24 | 12.22.12 (release note) | new |

### Revised hypothesis (high confidence that the old binaries use a different PRNG; medium confidence that this explains the failures)
- Every v0.3.x binary bundles **Node 4.x (V8 4.5)**. V8 switched Math.random to xorshift128+ in V8 4.9 (Node 6). On Node 4, Math.random is **MWC1616**, implemented in JS (math.js):
  `r0 = 18030*(r0&0xFFFF) + (r0>>>16); r1 = 36969*(r1&0xFFFF) + (r1>>>16); x = (r0<<16) + (r1&0xFFFF); return x/2^32`.
  This is recalled from the V8 source, not re-read this wave. Verify it against v8 4.5 `src/math.js` before relying on it.
- The w7a "old V8 s0+s1 ToDouble" model (Node 6-11) is therefore the wrong PRNG for v0.3.x binary users. That explains why the SAT attack in w8a could never work for them.
- Good news: MWC1616 is trivial to brute-force. The top ~5 bits of each draw (floor(x/2^32*26)) come almost entirely from the low 16 bits of r0 (plus a carry from r1). r0 and r1 are each independent ~2^32-state generators, so you can exhaust r0 (2^32) on the first 24 uppercase/lowercase letters in seconds to minutes of C, then fix r1. Note that Nyuu alternates `65+rand*26` / `97+rand*26`; every letter is one draw.
- Users who installed Nyuu with npm on a system Node have the Node of their own machine, so Node 6-11 (the s0+s1 model) or Node >=12 (the new model) are both possible.
- The dates fit: a v0.3.8 binary (Node 4.9.1) was the newest binary from 2018-05 to 2021-01. A user who kept using it, or who stayed on v0.3.8 through 2022, would produce every 2017-2022 failure. The **first 2023 success (2023-05-26) does NOT line up with a release date**: v0.4.0 (Node 12) was available from 2021-01 and v0.4.1 from 2022-05. So sanderjo probably upgraded his own install in 2023, rather than moving to a new release on its release day. **Recommended check:** run a 2017-2022 sanderjo ID set through an MWC1616 brute force (8 threads, 2^32 r0). If that fails, try the w8a s0+s1 model under a Node 6-11 assumption.
- The Message-ID format is otherwise unchanged across these versions (24 alternating U/l letters, '-', ms timestamp, '@nyuu'). v0.4.x zero-pads the timestamp to 13 digits. The charset is not a confounder.
- Other possible explanations if MWC1616 fails: a non-stock `--message-id` template using `${rand(N)}`. Nyuu's randStr uses the 62-char alphanumeric set via `Math.random()*62|0`. That gives a different leak per draw, and a solver that assumes 26-letter draws would fail. Retry/regeneration (`randomizeMessageID`) also consumes extra draws that the NZB never shows.

## Part 2: the base64url poster

### What it is not
- **Stock Nyuu templating:** `${rand(N)}` in bin/nyuu.js randStr uses `A-Za-z0-9` (62 chars) and has no `_` or `-`. Fixed N also gives fixed lengths. Only a custom JS function in a config file (config.js supports functions for the `Message-ID` and `From` headers) could produce this, and nothing in the docs ships one.
- **postie (javi11/postie v0.0.35, Go):** `GenerateMessageID` = `{rand62(32)}@{rand62(8)}.{rand62(3)}` and `GenerateFrom` = `{rand(14)} <{rand(14)}@{rand(5)}.{rand(3)}>`, using crypto/rand over the 62-char alphanumeric set. The `@8.3` host shape is reminiscent, but the charset (no `_-`) and the fixed lengths do not match. File: internal/article/article.go.
- **ngPost:** IDs are `<32hex>@ngPost` (the related 2021 post).
- **base64 of random bytes (token_urlsafe, randomBytes().toString('base64url'), Go RawURLEncoding):** unpadded base64 never produces a length of 4k+1 chars. The observed local length 25 and host length 5 and 9 are all 4k+1, so they **cannot** be base64 of random bytes. The observed lengths therefore rule out token_urlsafe and its equivalents. The generator picks characters one at a time from a 64-symbol alphabet (A-Za-z0-9_-), with a random length. A nanoid-style `customAlphabet`/`nanoid(size)` with a random size, or `random.choices(string.ascii_letters+digits+'-_', k=randint(...))`, fits.
- **Length relationship:** for 3 samples local = 2*host+11 (8->27, 7->25, 6->23), but 5->20 and 9->28 break it. Most likely both lengths are drawn independently from ranges (local ~20-29, host ~5-9) and the "tracking" is a small-sample coincidence. That is unconfirmed.

### The 68-article From rotation
No found tool has a native 68 cadence. Two candidate explanations, both low confidence:
- From is regenerated per NNTP connection/session or per batch. For example, a wrapper that restarts a poster per chunk of 68 articles, or a connection that posts N articles before reconnecting.
- 55 values over 3,304 segments = 60.07 on average. If it is exactly 68 per block, then 3,304 = 48*68 + 40, which would give 49 values, not 55. So the rotation is probably per file or per chunk boundary with some shorter blocks, for example per input file of roughly 68 articles (68 x 700 KB article ≈ 47.6 MB, or 68 x 750 KiB ≈ 50 MB, which suggests **~50 MB rar/7z volumes with From rotated per file**). This is the most plausible reading: one random From per volume. To check it, see whether the From changes line up with `<file>` boundaries in the NZB.

### Not identified
I found no public GitHub, npm, PyPI, or Go tool matching this fingerprint within the time box. The tool is most likely a private wrapper or a Nyuu config.js with custom functions (for example `'Message-ID': () => nanoid(20+rand(10)) + '@' + nanoid(5+rand(5)) + '.' + nanoid(3)`), or a modified fork. The related 2021 post with ngPost IDs and the same fixed From suggests the same uploader moved from ngPost to a scriptable tool.

## Confidence
- The bundled Node versions per tag are verified directly from the tagged source and the release notes.
- The MWC1616 claim for Node 4 is high confidence from memory of V8 history; it was not re-verified against the V8 source in this wave.
- The base64-length exclusion is mathematically certain, given that the samples are complete IDs.
- The 68 = per-volume explanation is a hypothesis.
