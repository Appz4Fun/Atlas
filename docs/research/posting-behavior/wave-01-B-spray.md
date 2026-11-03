# Wave 1 B: multi-group spray and poster rotation (agent ran 06:44-06:48Z)

1. ngPost: every random From shares one domain = Message-ID signature + ".com"; Message-ID <32hex@signature> (default ngPost). Only 4 headers, order From, Newsgroups, Subject, Message-ID; no Date, no User-Agent. Obfuscated Subject = bare UUID = Message-ID local part. https://github.com/mbruel/ngPost (NgPost.h randomStdFrom, NntpArticle.cpp header()) (4, high)
2. GROUP_POLICY=each_file in ngPost and postie: one random group per file. Produces the TRiToN pattern; merge by tool signature across groups. https://github.com/Hydro74000/ngPost/wiki/NgPost-conf (high)
3. Nyuu ([A-Z][a-z]){12}-\d{13}@nyuu Message-IDs rising evenly over a session; User-Agent Nyuu/x.y (HEAD). https://github.com/animetosho/Nyuu (4, high)
4. postie: Message-ID 32@8.3, From 14 <14@5.3>, Subject 32, all [a-zA-Z0-9]; full mode also randomizes yEnc name and Date (-6h); header order random per article (Go map); optional X-Nxg. https://raw.githubusercontent.com/kipsilabs/postie/main/internal/article/article.go (5, high)
5. nxg-upper: Message-ID sha256 40@21.3 hex from [NXGHEADER]:[data|par2]:[n]; same X-Nxg value (28-byte base64, no secret) in every article; empty Path. https://github.com/Tensai75/nxg-upper (5, med-high)
6. Bytes/Lines ratio from XOVER exposes line width (~line+2); default article sizes ngPost/Nyuu 716800, NewsUP 768000; last part smaller. (4, medium)
7. yEnc trailer: ngPost "=yend size= pcrc32=" lowercase unpadded hex, no part=/crc32=; Nyuu "size= part= pcrc32= crc32=". ngPost obfuscated still has real =ybegin name= unless the file was renamed. (5, high)
8. nZEDb MGR merges across groups only for whitelisted exact From; rotation defeats it. https://github.com/nZEDb/nZEDb/blob/0.x/nzedb/Binaries.php (high)
9. Server-added Path tail / NNTP-Posting-Host / Injection-Info / Organization stay constant per account; HEAD only, often stripped. (3, low-med)

Next: tool-classifier stage (regex on Message-ID/From shape) before collections; chain Nyuu timestamps; run LIST OVERVIEW.FMT and HDR Path/X-Nxg/Injection-Info on Omicron, Highwinds, Eweka.
