# W10-A: which posters rotate From per file with variable-length Message-IDs

Evidence: NZB cf32b14460 (one random From per file; From local part 20-29 chars in [A-Za-z0-9_-]; host 5-9 chars + "." + 3 chars).

## Per tool (source paths relative to /tmp/claude-0/.../scratchpad/w5b/)

- Nyuu (animetosho_Nyuu)
  - From: single config string, default "user <user@host>" (config.js:101). No per-file/per-article From hook found in lib/ (not fully traced).
  - ${rand(N)} token: randStr alphabet A-Za-z0-9, fixed N per token (bin/nyuu.js:227-233; token eval bin/nyuu.js:276-290).
  - Message-ID: regenerated per post, then truncated/padded to the original length (lib/article.js:250-258). Default ID is timestamp + fixed pattern (lib/article.js:199-209). Length is fixed per post, not random.
  - Verdict: can template random text, but no variable-length From or Message-ID by default. Partial only.

- ngPost, mbruel_ngPost
  - From: randomStdFrom(length=13), alphabet 62 chars A-Za-z0-9 (NgPost.h:599-609; sRandomAlphabet at NgPost.cpp:66). Form: 13 chars + "@" + "ngPost" + ".com" (NgPost.h:604-607).
  - --gen_from: per post, not per file (NgPost.cpp:1489-1496, 2091-2098; NgPost.h:517-518).
  - Default From per post when unset (NgPost.cpp:468-474).
  - Message-ID: sMsgIdFormat via QUuid-style IDs (NntpArticle.cpp:106-110, 139-141).
  - Verdict: From is fixed 13-char length, no '_' or '-'. Does not match.

- ngPost, Hydro74000_ngPost (fork)
  - Same randomStdFrom (NgPost.h:1145, 1359 region); From emitted at NntpArticle.cpp:266.
  - Message-ID: "<msgId@idSignature>" (NntpArticle.cpp:269).
  - Verdict: same as mbruel, no match.

- postie, kipsilabs_postie
  - Alphabet: 62 alnum, crypto/rand (internal/article/article.go:147-160).
  - GenerateFrom: "rand14 <rand14@rand5.rand3>", all fixed (article.go:181-200).
  - GenerateMessageID: "rand32@rand8.rand3", fixed (article.go:162-177).
  - Per-article From under ObfuscationPolicyFull (internal/poster/poster.go:1446, 1466-1472; also 1034-1036). DefaultFrom override (internal/config/config.go:286).
  - Verdict: per-article From, fixed lengths, no '_' or '-'. Structurally closest (rand@rand.rand3), but per-article not per-file and lengths do not vary.

- postie, javi11_postie (fork of kipsilabs)
  - Same generators: article.go:148-157 (62-char alnum), GenerateMessageID 163-177, GenerateFrom 181-200 (fixed lengths).
  - MessageIDFormat nxg|random (internal/config/config.go:126-132); random used at internal/poster/poster.go:1411.
  - Per-article From under full obfuscation (settings.json:300 says "random poster per article"; poster.go:1034-1036, 1466-1470).
  - Verdict: same as kipsilabs.

- NewsUP (../src/NewsUP): not checked (budget).
- Other posters via gh search code: not run (budget).

## Conclusion

No tool checked produces a From or Message-ID with a 64-char alphabet including '_' and '-', or a variable local-part length. Each generator either fixes lengths (kipsilabs, javi11, ngPost) or truncates to a fixed length (Nyuu).

Best structural match for cf32b14460: kipsilabs_postie / javi11_postie, because the format is rand@rand.rand3. They fail on per-file vs per-article From, alphabet, and fixed lengths. cf32b14460 is therefore probably a custom or unlisted poster. The random-length nanoid-like local part is not produced by any source reviewed here.

Open items: NewsUP From generator; a GitHub code search for the 64-char alphabet with '_-'.
