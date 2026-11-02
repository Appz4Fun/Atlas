# Wave 3 / Agent C: real samples from test fixtures (Haiku)
- sabnzbd.github.io tests/test_zip.nzb: iso-8859-1 decl, poster nzbtesterzzz@gmail.com, date 1495557485, subject `RefTestNZB: zip 8288838_ZIP_FILE.zip 24db65c8 - "8288838_ZIP_FILE.zip" yEnc (1/11)`, segments `bytes="350898" number="1"` -> `NzZUA.117908$rY1.16813@fx45.am4`, seg 2 `OzZUA.117909$rY1.86560@fx45.am4`.
  ORCHESTRATOR NOTE: this is a SERVER-ASSIGNED Message-ID (Highwinds-family "fxNN.<site>" injector; the client sent none). The middle counter goes up by one per article (117908, 117909), and the host names the injecting box (fx45, am4 = Amsterdam?). Consecutive counters on one host = same injection session, read from overview. Matches the posting-behavior thread's "<hex$0$n$hex@host>" backbone-overwrite note. Needs verifying across more samples.
- Nyuu help-full: Message-ID `${rand(24)}-{timestamp}@nyuu`, --keep-message-id, From `username <username@host>`, Subject `{comment} [{0filenum}/{files}] - "{filename}" yEnc ({part}/{parts}) {filesize} {comment2}`, --article-size 700K, line 128, --yenc-name {filename}.
- ngPost: `<uuid>@ngPost` (domain from -z), random From, a retry gets a new UUID.
- sabctools tests: `=ybegin part=1 line=128 size=123 name=-=DUMMY=- abc.par` (a comment, not a real article).
- github.com tree pages are blocked by robots for WebFetch.
