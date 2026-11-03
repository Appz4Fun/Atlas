# Wave 7C: survey of other posters for weak RNG (Opus subagent)

Time-boxed (~10 min) source read. Repos were shallow-cloned and grepped only; nothing was built or run.
Catalog tools (Nyuu, ngPost, NewsUP, postie/nxg, GoPostStuff, newsmangler, PowerPost, JBinUp, newspost) were left out except where a fork or wrapper changes behaviour.

## Bottom line

**No new tool with a recoverable PRNG in Message-ID was found.** The new tools fall into four groups:
1. **CSPRNG IDs** (pesto, usenet-posting, nntpPoster fallback). These cannot be linked through the RNG.
2. **Deterministic or structured IDs that link posts directly, with no RNG attack needed:** geri (HMAC of file+part), the keimpema/Spottarr `Usenet` .NET library (one GUID per file, repeated in every segment ID), and isc-usenet Upstream (float `time.time()`).
3. **Server-assigned IDs** (SuperPoster, nntpPoster by default).
4. **Wrappers** that call Nyuu or ngPost and add their own randomness for groups or subjects (transmitter, NZBPostarr, fastuploader, Lift-Cup, usenet-upload.py, Nyuu_GUI). Their IDs come from the wrapped tool, so the existing Nyuu V8 and ngPost rand() solvers apply.

## Table

| Tool / repo | Lang | Random header fields | RNG and seed | Values to recover state | Session linking |
|---|---|---|---|---|---|
| franzopl/pesto | Rust | Msg-ID `<{u128:032x}@domain>`; From (random identity per run when blank); obfuscated subject/filename (`obfuscate=full/article`); random lengths 10..30 and 8..22 | `rand` 0.9: `rand::random()` / `rand::rng()` = ThreadRng (ChaCha12, OS-seeded, reseeds) | Not recoverable | Not through the RNG. Run-wide From and fixed `message_id_domain` link a run. Msg-ID is 32 lowercase hex characters with no dashes, a format fingerprint (crates/pesto/src/article.rs:16-27) |
| Brennerofhell/usenet-posting | Rust | Msg-ID `<uuid-v4@domain>`; optional random Msg-ID domain from a fixed list; fake identity "First Last <email>"; spoofed User-Agent picked from a list (mode 4 = per post) | `uuid::Uuid::new_v4()` (getrandom). Identity fields use `bytes[i] % len`, which gives a slight modulo bias | Not recoverable | Fingerprint is the dashed v4 UUID together with a realistic-domain list and a fake UA list (Xnews/5.04.25, NewsLeecher v8.0 Final, "Usenet Wire/1.4.2 (Windows)" ...) (src/identity.rs, src/poster.rs:245-262) |
| realNeodome/geri | Elixir | Msg-ID = `<b64url(HMAC-SHA256(upload_key, "GERI2"‖enc‖filename‖part‖total‖SHA256(body)))@{crc32hex}.geri>` | Deterministic | n/a | **Deterministic.** Reposting the same file+part with the same key gives the same ID, so the server dedups (435/441) rather than taking a new post. The domain carries the yEnc **part CRC32** in clear (`xxxxxxxx.geri`), which links overview entries to body CRCs. A key that is the same across uploads makes reposts identical (lib/geri/article.ex:94-126) |
| keimpema/Usenet and fork Spottarr/Usenet (NzbBuilder) | C#/.NET library | Msg-ID `part{n}of{N}.{guid:n}@{messageBase}`, default base `unknown.com` | `Guid.NewGuid()` (CSPRNG), **one GUID per file** | n/a | **Strong intra-file link:** every segment of a file shares the same 32-hex GUID, and the ID states the part number and total. Subject `"name" yEnc (01/NN)` (Usenet/Nzb/NzbBuilder.cs:157-170) |
| isc-usenet/Upstream (riverup) | Python 2 | Msg-ID `"%f@%s" % (time.time(), ctx)`, default domain `riverup` | **No RNG: wall clock, microsecond float** | 0. The ID is the timestamp | Posting time is exposed to the microsecond. IDs are monotonic within a session, can collide across threads, and link a session by time adjacency and the `@riverup` domain (upstream.py:304-312, 529) |
| Fuzion24/SuperPoster | Java | No Message-ID header sent (server-assigned). Static header `Organization: turboPoster` | none (SecureRandom only for TLS) | n/a | Fingerprint: the `Organization: turboPoster` header (with its trailing space), and a server-assigned ID pattern. Subject is `commentOne "name" yEnc (i/n) commentTwo` (net/usenet/NNTPclient.java:232-235) |
| boranblok/nntpPoster | C#/.NET Fx | Msg-ID taken from the server's 340 response when present, otherwise `<Guid.NewGuid():N@serverhost>`. Obfuscated name = `Guid.NewGuid():N`. Password = GUID or RNGCryptoServiceProvider string | GUID and RNGCSP are secure. `new Random()` (TickCount seed, Knuth subtractive) picks **only the length** of the random string | Length alone gives at most a few bits. TickCount seed is brute-forceable (2^32) but leaks almost nothing | Fallback ID contains the **NNTP server hostname** (PostingNntpClient/SimpleNntpPostingClient.cs:152-160; Util/RandomStringGenerator.cs:33-36) |
| x-cord/transmitter | Python wrapper of Nyuu | Nyuu `--from ${rand(20)}@${rand(20)}.${rand(3)} --subject ${rand(20)} --filename ${rand(20)}`, group `alt.binaries.boneless` | **Nyuu `${rand(N)}` = V8 Math.random**, already solved by our v8-solver | As for the Nyuu solver | Multi-account: several Nyuu processes, each with its own V8 state. The fixed NZB subject template appears in NZB only (transmitter.py:15,157) |
| polyn0mial/NZBPostarr | Python wrapper of Nyuu | Group choice `random.sample(pool, 3)` from a default pool (boneless, mom, moovee, teevee) | Python `random` (MT19937, seeded from os.urandom at import) | 624×32-bit outputs are needed. A 3-of-4 sample leaks about 4.6 bits, so recovery is impractical | Group triple per job is a weak per-job feature. Msg-ID and other headers come from Nyuu, so they fall under the existing solver (logic/pipeline/posting.py:228-235) |
| Vladimir-TB/ngpost- (fork) | C++/Qt | Same as upstream ngPost: Msg-ID `QUuid::createUuid()` (secure); From `randomStdFrom` 13 chars from a 62-char alphabet via `std::rand()%62`; group `std::rand()%nbGroups` | `std::srand(QUuid::createUuid().data1)`, a 32-bit seed | Same attack as ngPost: brute-force the 32-bit seed against a From local-part (13 chars, about 77 bits, so one From is enough) | No change from the catalog's ngPost entry (src/NgPost.cpp:328, NgPost.h:552,629-637) |
| franzopl/fastuploader | shell/AppImage wrapper of ngPost | ngPost.conf | ngPost | ngPost | Same as ngPost |
| midgetspy/Lift-Cup | Python wrapper of newsmangler | — | newsmangler | — | Same as the catalog entry |
| dgsmiley18/usenet-upload.py, marduk191/Nyuu_GUI, blackrosexx/Nyuuautopost, mpldr/UsenetPostAutomation | wrappers of Nyuu | — | Nyuu V8 | — | Same as Nyuu |
| godevindahouse/nzbup | Go Telegram bot | Calls an external poster with `--gen_name`. Its own job IDs use crypto/rand | — | — | Wraps an existing poster |
| rnavarro/Py-Post | Python 2 | NZB only. No random header code found | — | — | — |

## Notes against the RNG list in the brief

- **Go math/rand before 1.20 (seed 1 when unseeded):** no new Go poster found that uses math/rand. The new Go code (nzbup) uses crypto/rand. postie and GoPostStuff are already covered.
- **Java java.util.Random:** the only Java poster (SuperPoster) has no RNG in its headers.
- **.NET System.Random (TickCount):** used only in nntpPoster, and only for a length. Nothing else in .NET posters is weak. Guid.NewGuid is a CSPRNG.
- **Python random (MT):** used only for group choice in NZBPostarr, which leaks too little to recover the state.
- **Rust:** both Rust posters use ThreadRng or uuid v4 (secure).

## Most useful for linking

1. **geri:** the CRC32 sits in the Msg-ID domain, and IDs are deterministic per (key, file, part, body).
2. **keimpema/Spottarr `Usenet` NzbBuilder:** one GUID per file appears in every segment ID.
3. **Upstream:** the Msg-ID is a microsecond timestamp.
4. **Wrappers of Nyuu or ngPost** can be solved with our existing solvers, and they add their own fingerprints: transmitter's `rand(20)@rand(20).rand(3)` From shape, and NZBPostarr's 3-group sets.

## Not covered (time box)

No PyPI sdist search, since PyPI has no search API. No crates.io or pkg.go.dev crawl beyond GitHub language searches. The Tensai75 repos were not reached because the GitHub user endpoint is blocked in this session.
