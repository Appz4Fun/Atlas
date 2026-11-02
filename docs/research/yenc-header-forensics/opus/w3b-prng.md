# Wave 3B: PRNG feasibility (Opus subagent, local experiment)
V8 Math.random state recovery from floor(x*62) — TESTED, WORKS (Node v22 / V8 12.4):
- Model: state0>>12 mantissa, 64-entry cache filled by stepping, served cache[63]..cache[0].
- Each floor(x*62) fixes ~4-5 leading bits (common binary prefix of [k/62,(k+1)/62)).
- Pure-Python GF(2) Gaussian elimination (no z3): 24 outputs rank 100, 28 not enough, 32 outputs full rank 128, solved in 0.2 s; predicted 200 outputs exactly across 3 cache refills.
- Cache offset ambiguity resolved by testing all 64 offsets against data that crosses a block boundary.
- One Nyuu-Obfuscation article (~111 draws) crosses a boundary -> full state + offset; predict next (and, inverting xorshift128+, previous) articles' From/Message-ID/Subject.
- TODO: map Nyuu charset order; allow a draw window for interleaved Math.random consumers (Date jitter, connections).
- Code: research/yenc-header-forensics/opus/v8-solver/ (gen.js, solve.py).
ngPost:
- Correction: obfuscated mode makes From per article in NntpArticle::header() (NntpArticle.cpp L136-151; PostingJob.cpp L691 sets _from=nullptr) on the posting threads; non-obfuscated mode one From per job on main thread (NgPost.h L514-521).
- Subject == Message-ID UUID local part in obfuscated mode (NntpArticle.cpp L146): no RNG needed to fingerprint.
- Charset A-Z a-z 0-9 (NgPost.cpp L66), 13 chars %62.
- Seed-1 MSVC LCG From sequence computed: p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv, k7DMPDONXrwD2, yVLZrHBio74so (matches W2 prediction). Windows build compiler (MSVC vs MinGW) not confirmed; both CRTs keep per-thread rand (from memory). Web search for the strings not yet done.
- Group per file uses the main-thread std::rand stream too (PostingJob.cpp _initPosting, from sibling thread) -> a recovered glibc seed predicts which group each file went to.
RNG survey of Python/Go/old Qt: not done.
