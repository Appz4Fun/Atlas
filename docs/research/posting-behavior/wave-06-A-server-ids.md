# Wave 6 A: server-assigned Message-IDs beyond Highwinds (data: server-ids/segs.tsv = 72,334 segments from 261 public NZBs on GitHub)

| format | example | fields | time | match | evidence |
| Diablo dreaderd (@news.astraweb.com, @news.usenetserver.com, @news.free.fr, @uploadreader.eweka.nl) | 4f9a3e8a$0$14349$c3e8da3$33a0879d@news.astraweb.com | <time$seq$pid$hosthash[$x]@cluster>: time = 8 hex unix s; seq = per-process counter reset each second (0-2 seen); pid = dreaderd process; hosthash = shash(front-end hostname); domain = cluster. Astraweb adds a 5th hex field constant per (front-end, pid). | hex unix | 521/602 within 10 min (Astraweb lags NZB date -1..+36 s; usenetserver sample off exactly 3600 s = TZ bug in NZB; free.fr up to +967 s vs first-segment date) | github.com/jpmens/diablo dreaderd/subs.c:260 "<%08lx$%d$%d$%x@%s>"; nzbget testdata |
| Giganews platform (@giganews.com; also comcast.com, speakeasy.net, cavtel.net) | R72dnSXSkbrxWUHWnZ2dnUVZ_qmdnZ2d@giganews.com | 32 url-safe b64 = 24 bytes; XOR every byte with 0x9D; six LE uint32: w0 small id (<=0x7533, process/server?), w1 32-bit id not time-rising (+1 between two posts 415 s apart from same w0/w4 -> counter?), w2 = UNIX TIME, w3 = 0, w4 = 0x??63c4d8 (low byte varies: front-end?), w5 = 0 or 0x9d9d9d9d (tail nZ2d vs AAAAA) | XOR 0x9D, LE, unix | 9/10 within 14 s of Date (4 exact); miss +45.5M s (cavtel) | spam/ham corpora: vu-aml/adlib trec07p, NetsecExplained ham/. 68edne9R...@giganews.com = 1343162630 (2012-07-24 20:43:50Z) |
| newsmangler (client) | 1311190935.12105.1@eu.news.astraweb.com | <%.5f time.time()>.<partnum>@<configured server host> | decimal unix | 864/864 within 10 min; last field == NZB segment number | newsmangler postmangler.py:287 |
| Usenet.farm | sLxLvNvawWSmfxcuLOgxyZxloVYdrnrv@news.usenet.farm | 32 random [A-Za-z] | none | - | godver3/mediastorm testdata |
No samples found: UsenetExpress, Omicron/Newshosting, Abavia, Easynews, Supernews. The INN "$1@" style seen was Diablo.

XOVER-only use:
- Diablo: injection second + front-end (hosthash + cluster) + process (pid). A multi-connection upload = N pids (30 in one sample) on 1-2 hosthashes in a tight window -> cluster on (cluster, hosthash, pid set, window).
- Giganews: exact time (w2) + w0/w4 ids; w1 maybe a counter (needs same-session samples).
- newsmangler: .N = segment number; sort by float time, reset at N=1 -> file boundaries; domain = provider used.
- One NZB mixed Diablo IDs (news.astraweb.com) with newsmangler IDs (@eunews.blocknews.net): an upload can span formats.
Next: pin down Giganews w0/w1/w4 from same-session archives; find UE/Newshosting/Easynews/Supernews/Abavia IDs in mail corpora and run XOR/endianness/epoch sweeps.
