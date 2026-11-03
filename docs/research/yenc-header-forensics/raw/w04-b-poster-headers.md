# Wave 4 / Agent B + orchestrator git clones: poster header sets (VERIFIED from source, cloned 2026-11-02)
GoPostStuff (agent, raw article.go): order From, Newsgroups, Message-ID `<UnixNano$gps@gopoststuff>`, X-Newsposter `gopoststuff <ver> - https://github.com/madcowfred/gopoststuff`, Subject `%s [%d/%d] - "%s" yEnc (%d/%d)`. No Date or User-Agent.
NewsUP (github.com/demanuel/NewsUP lib/NewsUP/Article.pm):
- header order From, Newsgroups, Subject, Message-ID, then extra X- headers (l.108-116).
- =ybegin `part= total= line=128 size= name=`; =ypart `begin=+1 end=`; **=yend `size= pcrc32=` with NO part= and no crc32=** (l.308-314). This is a unique trailer.
- random From when none is set: `<8 letters> <0-17 letters@1-6 letters.???>` (l.324-328). Random strings are letters only.
newsmangler (madcowfred/newsmangler):
- =ybegin `part= total= line=128 size= name=`, =yend `size= part= pcrc32=` (article.py l.63-75).
- Message-ID `<%.5f.%d@server-hostname>` + X-Newsposter (postmangler.py l.287-288).
- **asyncnntp.py l.268-275: if the server's 340 reply to POST carries a suggested <Message-ID>, newsmangler REPLACES its own ID with the server's.**
sanguinews (tdobrovolskij/sanguinews lib/sanguinews/nntp_msg.rb):
- header order From, Newsgroups, Subject, X-Newsposter `sanguinews v<ver> (ruby <ver>) - https://github.com/tdobrovolskij/sanguinews`, optional X-No-Archive: yes, Date `strftime('%a, %d %b %Y %T %z')` (numeric zone). **It sends no Message-ID, so the server assigns one.** Ruby `puts` writes LF, so bare-LF headers are possible, but the CRLF translation in its nntp layer was not checked.
- single-part: `=ybegin line=128 size= name=` / `=yend size= crc32=`. Multi-part: pcrc32 on every part, crc32= only on the last.
nntpPoster (boranblok/nntpPoster, C#):
- Date `ddd, dd MMM yyyy HH:mm:ss zzz (UTC)` with the zone colon removed, giving "+0000 (UTC)" (SimpleNntpPostingClient.cs l.106-107).
- Message-ID = **the ID extracted from the server's POST response** (ExtractMessageID) unless overridden. Fallback `<GUID-N@server-address>` (l.105, l.160). Date then Message-ID order. Obfuscated names and passwords are GUID "N" (32 hex).
- single-part `=ybegin line= size= name=` / `=yend size= crc32=`.
Backbones stripping User-Agent/X-Newsposter: no evidence either way.
