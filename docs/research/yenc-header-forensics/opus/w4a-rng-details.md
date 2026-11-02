# Wave 4A: RNG details (Opus subagent, cloned repos)
Nyuu-Obfuscation v0.4.2-Obfuscate:
- charset 'A-Z a-z 0-9' (bin/nyuu.js:227-232), one draw/char; tokens replaced left to right (278-279).
- Build block per article (lib/article.js MultiEncoder.generate 92-166): [first part of file: 1 draw name length floor(r*17)+16 (112)] -> msgid 43 draws (discarded later) -> Subject 32 -> From 36 (14,14,5,3) -> Date 1 draw floor(r*21600000) ms (47-53,154-155). =112 (113 first part).
- Send: post.randomizeMessageID() (article.js:273-283, nntp.js:828) redraws 43 -> WIRE msgid is drawn at send time; retries +43; XREPLIC +1.
- One V8 isolate (sockets in worker isolate don't consume); builds run ahead by articleQueueBuffer=min(round(conns*0.5),25) (config.js:176). Gap N->N+1 ~155 draws (112 build + 43 send), interleaving varies.
- Subject+From = 68 consecutive floor(x*62) outputs from one article -> enough for solver; search forward in 43/112/113/155 steps for neighbours (not yet run).
NewsUP (Perl):
- charset 0-9A-Za-z (+ - _ . $ unless alphan) (Utils.pm:674-696); From rand8 <...> (Article.pm:324-329).
- OBFUSCATE: per-article random style among gopoststuff/newsmangler/nyuu/JBinUp/JBinDown/PowerPost/own (Utils.pm:710-800).
- Own style "$sec$usec.".base36(rand("$sec$usec"))."@".base36("$sec$usec").".newsup" (743-748): rand(~2^50) leaks ~all 48 bits of drand48 -> full state from ONE id; all IDs of a file generated in one loop (bin/newsup.pl:329), returned in hash order. Perl>=5.20 uses own drand48 everywhere (inference).
ngPost:
- Windows release is MinGW (libgcc_s_seh-1.dll, libstdc++-6.dll, libwinpthread-1.dll, Qt 5.12.5 in release/libs/libs_win7_64bit.zip); msvcrt rand per-thread LCG (inference) -> seed-1 hypothesis likely holds.
- srand(QUuid data1) since v4.15; before: srand(currentMSecsSinceEpoch) (NgPost.cpp:314, release_notes.txt:49) -> pre-4.15 seed is a ms timestamp near the post Date: trivially brute-forced.
- From per article in NntpArticle::header() (:144) via write() (:132) on Poster _connectionsThread (Poster.cpp:59). Retry = +13 draws.
- Web search for p1KaLmIg2kB7f / Zp5Tm1qbi6dss: no hits (NZB indexers unreachable).
- msgid/subject QUuid (CoCreateGuid on Windows).
Survey (memory): Python MT needs 624 outputs (heavy with 6-bit leaks); Go math/rand pre-1.20 default seed 1 unless Seed called; Qt<5.10 QUuid on Unix used qrand.
Leads: run Nyuu-Obf against test/_nntpsrv.js stub and verify prediction; drand48 inverter for NewsUP; ngPost seed-1 confirm under Wine / glibc brute forcer.
