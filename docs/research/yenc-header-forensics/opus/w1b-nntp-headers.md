# Wave 1B: NNTP header forensics (Opus subagent)
A Message-ID grammar:
- Nyuu: ^([A-Z][a-z]){12}-\d{13}@nyuu$ ; ms timestamp = genTime, same as Date header; retries keep original genTime (Nyuu lib/article.js createMessageId ~199-229).
- ngPost: <32 hex QUuid v4>@ngPost (sig configurable); v4 nibble pos13='4', pos17 in [89ab]; obfuscated Subject == msgid local part (NntpArticle.cpp, NgPost.h).
- GoPostStuff: <{UnixNano}$gps@gopoststuff>, X-Newsposter: gopoststuff <ver> (article.go).
- newsmangler: <%.5f.%d@%s> = epoch.10us . partnum @ NNTP server hostname (leaks provider); X-Newsposter newsmangler (postmangler.py 287,317).
B From: Nyuu default "$USER <$USER@hostname>" (+.localdomain), fallback user@nyuu.uploader (config.js:101) -> per-machine uploader ID. ngPost random From [A-Za-z0-9]{13}@<sig>.com, std::rand()%62 seeded srand(QUuid data1) 32-bit.
C Header order: Nyuu Message-ID, Subject, From, Newsgroups, Date, Path(empty), User-Agent Nyuu/<ver>. ngPost From, Newsgroups, Subject, Message-ID only (no Date -> server Date). GPS From, Newsgroups, Message-ID, X-Newsposter, Subject. newsmangler From, Newsgroups, Subject, Message-ID, X-Newsposter (possible extra CRLF bug). Needs HEAD, not OVER.
D Body: Hydro74000/ngPost fork v5.6 fixed pcrc32 padding + trailing ws escaping + mt19937 msgids (https://github.com/Hydro74000/ngPost/pull/20) -> version fingerprint. GPS pcrc32 %08X uppercase. :lines server-independent, :bytes includes injected headers so differs per server -> twin key uses part size+:lines, not :bytes.
E Injection: NewsGuy NNTP-Posting-Host per-user hash token (*.newsdawg.com); Organization/X-Complaints-To/X-Usenet-Provider/X-Received-Bytes per provider; first Path element + Injection-Date TZ = injecting provider. X-Trace = host epoch pid IP (big-8.org/wiki/Headers). Current Omicron/Abavia/UsenetExpress/Eweka unverified.
Most surprising: Nyuu default From leaks OS user+hostname; ngPost obfuscated Subject==msgid UUID.
Leads: ngPost std::rand PRNG seed recovery (MSVC per-thread seed 1?) to link a process's From strings; real HEAD samples per backbone; grammar for JBinUp/Camelsystem/PowerPost/newspost/yEncBin/postie/forks.
