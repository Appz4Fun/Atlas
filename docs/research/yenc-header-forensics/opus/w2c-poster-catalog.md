# Wave 2C: posting-tool catalog (Opus subagent)
NewsUP moved to demanuel/NewsUP (lib/NewsUP/Article.pm, yEnc.pm, Utils.pm):
- native msgid `<sec><usec>.<B36 rand LSD-first>@<B36>.newsup`; From `NewsUP <NewsUP@somewhere.cbr>`; obfuscated From `rand8 <...>`.
- headers From, Newsgroups, Subject, Message-ID, then X- user headers; NO Date, NO User-Agent.
- =ybegin part total line=128 size name / =ypart / =yend size pcrc32 (no part=, no crc32); pcrc32 %x lowercase UNPADDED; article 768000; same end-of-line ws bug as ngPost (ngPost derived); empty line before =yend; article Subject has NO "yEnc": `[i/n] - "file" (p/P)`.
- -obfuscate: picks fake msgid grammar among nyuu rand12-<sec><usec>@nyuu, powerpost part{1-98}of{1-98}.rand22@camelsystem-powerpost.local / @powerpost2000AA.local, newsmangler sec.usec.rand1024@<28 provider hosts>, gopoststuff sec.usec$gps@gpoststuff (typo!), jbinup rand32@JBinUp.local, jbindown rand32.sec-usec<rand>@JBinDown.local, generic. Msgid alone can't identify tool; yEnc layer still does.
postie (javi11 -> kipsilabs/postie, Go; internal/article/article.go, config.go; rapidyenc encoder mnightingale/rapidyenc encoder.go):
- msgid [A-Za-z0-9]{32}@[A-Za-z0-9]{8}.[A-Za-z0-9]{3} crypto/rand, or nxg mode; From [A-Za-z0-9]{14} <{14}@{5}.{3}> per article.
- header ORDER RANDOM per article (Go map iteration).
- =yend size part pcrc32=%08x, no crc32=; line 128; article 750000; Date RFC1123Z "+0000"; RandomDateWithinLast6Hours.
Nyuu-Obfuscation (Antidote2151): same msgid/From templates as postie; yEnc name = crypto.randomBytes(16..32).hex (even-length lowercase hex 32-64, fixed length per file); Date toUTCString "GMT" backdated random 0-6h; Subject rand32; Nyuu header order fixed.
NxG (javi11/nxg nxg.go, Tensai75/nxg-upper): msgid ^[0-9a-f]{40}@[0-9a-f]{21}\.[0-9a-f]{3}$ = H=sha256("<b64hdr>:data|par2:n"); obfuscated Subject = sha256(H) 64 hex; From from P=sha256(Subject): `P[10:15] <P[10:25]@P[30:45].P[50:53]>` -> display name = first 5 chars of local part. Self-verifying with 3 hash checks. X-Nxg header encrypted fileNo:total:name:type:size.
ngPost Hydro74000: resume reposts unconfirmed article under new msgid (same yEnc part twice).
Not reached: JBinUp, PowerPost/yEnc32, newspost, Newsbin, Grabit, UE, Spotweb.
Leads: rest of NewsUP + nxg-upper encoder; postie vs Nyuu-Obfuscation separators (Date suffix, header order, name charset); Wayback for Windows posters + yenc draft trailing spaces + Spotweb posting.
