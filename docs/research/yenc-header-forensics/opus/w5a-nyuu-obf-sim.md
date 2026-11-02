# Wave 5A: Nyuu-Obfuscation end-to-end (faithful re-implementation on real V8; fork code NOT run)
- Source re-checked (fork master): chars[(Math.random()*62)|0] bin/nyuu.js:227-232; defaults 841-843; build order in lib/article.js generate(): name-length draw (first part, line 112) -> createMessageId 43 (~136) -> Subject 32 -> From 36 -> Date 1 floor(r*21600000) (47-52,155); send nntp.js:828 randomizeMessageID 43 per attempt (+XREPLIC 1 at 832, not modelled). Wire ID always 45 chars.
- Result: from ONE article's Subject+From (68 chars) solve2.py recovered state (~0.1 s, pure Python, GF(2) + 64-offset search + exact xorshift128+ inverse) and predicted ALL 60/60 articles' Subject, From, wire Message-ID, build-time ID, and Date offset, backward and forward, for Q=1,2,3, warm-ups 0..101, observed article 0/30/59, with and without a retry (~25 runs). Discarded first-attempt ID of a retry also found.
- Gaps between consecutive Subjects: 155 normal, 156 at new file, 112 at file/queue edges, 198 at a retry. Wire ID sits 149 (Q=1) / 304 (Q=2) draws after its own Subject.
- Subject alone (32 chars) is NOT enough (rank-deficient; false offsets, 3-19/60 hits).
- Limits: assumes no other Math.random consumers mid-run; unverified on real posts and on which Node/V8 versions the real tool used.
- Files: nyuu-obf-sim/ (sim.js, solve2.py, solve_subjonly.py).
- Next: real Nyuu-Obf post with >=20 segments: Subject+From from overview, predict the NZB's Message-IDs.
