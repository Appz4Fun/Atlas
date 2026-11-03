# Wave 7A: Nyuu 48-draw gap + old-V8 Math.random (why 2017-2022 Nyuu IDs do not solve)

## 1. The 48-draw gap is explained by the Nyuu source
- `lib/article.js` `Post.prototype.createMessageId` draws Math.random 24 times (65+x*26 / 97+x*26 alternating) + `-<genTime>@nyuu`. https://raw.githubusercontent.com/animetosho/Nyuu/master/lib/article.js (lines ~199-228)
- Build time: `article.js` ~line 115 `post.messageId = post.createMessageId(post);` -> 24 draws when the article is built (that ID goes into the buffered post).
- Send time: `lib/nntp.js` ~line 828 in `doPost()`: `// we hard-code a message-id, since it's best to make it unique for retries` / `if(!post.keepMessageId) post.randomizeMessageID();` -> 24 more draws on **every POST attempt**; `randomizeMessageID` (article.js ~250) overwrites the ID in the buffer. The published / NZB ID is the send-time one (uploadmgr.js `post.nzbSeg(post.messageId)` after posting).
- So per article: 24 discarded build draws + 24 published send draws = 48. Gap 72 = a retry (one more randomize). Gap 24 = two sends happen back to back with no build in between (build is pipelined/buffered ahead and runs on a different schedule than sends across connections), so the interleaving varies.
- Other per-article Math.random: only `XREPLIC` mode (nntp.js ~832, `Math.random()*0x7fffffff`), not used for normal POST. `bin/nyuu.js` ~231 has a random-chars helper (used for `${rand(N)}` style option tokens / obfuscation, not per default article).
- `keepMessageId` defaults to false except for raw posts (bin/nyuu.js ~1087), so with stock settings the stream is 48/article.
- This code is identical in tags v0.3.0, v0.3.4, v0.3.8, v0.4.0, v0.4.1, v0.4.2 (each article.js has the same 24 draws; each nntp.js calls randomizeMessageID in the post path; v0.3.8 adds the keepMessageId guard). v0.3.8 omits the 13-digit zero-padding of genTime but the format is the same. **The Nyuu version is not the reason old posts fail.**

## 2. V8 Math.random history (checked in V8 source per tag, raw.githubusercontent.com/v8/v8/<tag>/src/base/utils/random-number-generator.h)
| V8 tag | ToDouble |
|---|---|
| 4.9.385, 5.1.281, 6.2.414, 6.8.275, 7.0.276 | `random = ((state0 + state1) & kMantissaMask) \| kExponentBits; return bit_cast<double>(random) - 1;` (low 52 bits of s0+s1) |
| 7.1.302, 7.2, 7.3, 7.4.288, 8.4, 9.4, 10.2.154, 11.3.244, 12.4.254 | `random = (state0 >> 12) \| kExponentBits; return bit_cast<double>(random) - 1;` |
- So the switch is between V8 7.0 and 7.1 (late 2018), NOT 2022-2023. No ToDouble change found in 2022-2023 (10.2 = Node 18, 11.3 = Node 20, 12.4 = Node 22 all identical). No change to `state0>>11 / 2^53` exists up to 12.4.
- Node mapping: Node 8 = V8 6.x, Node 10 = V8 6.8, Node 11 = V8 7.0 -> **old (s0+s1)**; Node 12 = V8 7.4 and later -> state0>>12.
- xorshift constants (23,17,26) and the LIFO 64-entry cache are the same in old versions (6.2 `builtins-math-gen.cc` MathRandom decrements the index; 7.1 `src/math-random.cc` RefillCache fills 0..63, index starts at 64). Background: https://v8.dev/blog/math-random (xorshift128+ since V8 4.9, Dec 2015).
- Hence the most plausible cause for the 2017-2022 failures: sanderjo's Nyuu ran on Node <= 11 (e.g. an old Nyuu Windows/nexe binary bundling Node 8/10) until early 2023, when he switched to a build on a newer Node. Not verified which Node the release binaries bundle (next step: check the Nyuu release notes / binary `process.versions`, without running it -- e.g. strings on the exe).
- Caveat: if a 2019-2022 post had been made on Node >= 12, the original solver should have solved it; it solved none, which supports the "old Node" explanation but does not prove it.

## 3. Old-conversion solver: nyuu-id-solver/nyuuid_solve_old.py (original untouched)
- Model: output_g = floor(((A_{g-1} + A_g) mod 2^52) * 26 / 2^52), where A_g is the new state1 after step g (old state1 + new state1 = state0+state1 after the step), with the LIFO cache. Includes `--synth N gap seed`: my own Python reimplementation of old V8 + Nyuu (build ID discarded + send ID), and a z3 solver per cache offset, with gaps 24/48/72 between two IDs.
- Result: **z3 does not solve it within limits.** Synthetic test (30 IDs, gap 48): z3 (BitVec, 52-bit extract + interval constraints, 48 draws) returned `unknown` after a 120 s timeout even at the **correct** cache offset; with the qfbv tactic, 96 draws (4 IDs), and a 6-bit top window it was still `unknown` after 150 s. With 64 offsets x 3 gaps x pairs this is far too slow. Each draw gives only ~4.7 bits through a full 52-bit carry chain, which is much harder than the known z3 attacks that use whole doubles.
- So no real 2017-2022 NZB was solved. Five failed NZBs were downloaded into the scratchpad for testing (german-umlauts-10MB, ExclamationMarkInResultingFile, reftestnzb_question_mark, ecoji, with-passsssword from https://github.com/sanderjo/NZBs) but not run, because the synthetic test already failed.

## 4. Cross-NZB linking
Not done (time box). For new-V8 posts it should work directly: solve state from NZB A, regenerate forward ~(articles_A*48 + slack) draws and search for NZB B's IDs (nyuuid_solve.py `served()` with large BF). Same-process uploads only (Nyuu runs one process per invocation; a new process seeds V8 fresh).

## Next steps
1. Old conversion, better attacks: (a) bit-blast to CNF and use CryptoMiniSat/kissat with XOR clauses (xorshift is pure XOR, which suits CMS); (b) guess-and-determine: guess the carry into bit 47 per output (biased) plus the top bits of A_0, then the top ~4 bits of each A_g become linear GF(2) equations, so prune by rank once >128 equations, with a beam search over ambiguous ranges; (c) use many more draws (whole NZB, gap-48 pattern) to over-determine.
2. Confirm the Node version: look at the Nyuu release assets (nexe Node version) for 0.3.x vs 0.4.x and the dates sanderjo switched.
3. Cross-NZB test on the 19 solved 2023+ NZBs: pairs posted the same day -> regenerate the stream forward.
