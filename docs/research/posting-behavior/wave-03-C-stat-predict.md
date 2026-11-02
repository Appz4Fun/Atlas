# Wave 3 C: predicting sibling Message-IDs + STAT probe cost (07:52-08:00Z)

Per format:
1. NxG (postie message_id_format nxg, nxg-upper): EXACT once the 28-byte header is known. id = sha256("{hdr}:{data|par2}:{n}") split [0:40]@[40:61].[61:64]; hdr = base64(rand21 with ":{D}:{P}" over its tail); rand from rand.NewSource(time.Now().UnixNano()), charset[r.Intn(len-1)] off-by-one => 'Z' never appears (fingerprint). postie: one header per file, GenerateNXGHeader(numSegments,0), numSegments from yEnc size/part. Unknown = UnixNano seed anchored on first Date: 15.5 us/seed/core benchmarked; +-1 s Linux ns clock ~4.3 core-hours (~8 min on 32 cores); macOS us clock ~15 s; Windows coarse. Then N STATs. nxg-upper with unobfuscated yEnc name: name= is rand21, ~1e7 sha256 offline. Subject = sha256(segmentHash), checkable. https://raw.githubusercontent.com/javi11/nxg/main/nxg.go , utils.go, postie poster.go ~L1364. (high)
2. newsmangler <%.5f.%d@host>: all Article objects built in one loop, part k+1 ~1-3 ticks of 10 us later; <~10 STATs per sibling; Windows Python 15.6 ms clock often same ts, vary part number. postmangler.py L283-287. (high)
3. GoPostStuff <UnixNano$gps@gopoststuff>: gaps tens of ms at ns resolution, 1e7-1e8 STATs per sibling: impractical (subjects not obfuscated anyway). (high)
4. NewsUP native "$sec$usec.$b36rand@$b36time.newsup": time narrowed (ms), ~50-bit drand48 random part (PRNG team), part->ID order shuffled. Obfuscate mode posts each article to a random SUBSET of groups. NewsUP also IMITATES other tools: {12rand}-{sec}{usec}@nyuu, {sec}.{usec}$gps@gpoststuff (misspelt), {sec}.{usec}.{0-1023}@{28 provider hosts}, {32}@JBinUp.local, {32}.{sec}-{usec}{rand}@JBinDown.local, part{1-98}of{1-98}.{22}@camelsystem-powerpost.local / @powerpost2000AA.local. Only public descriptions of JBinUp/PowerPost formats. Utils.pm L710-830. (high)
5. Not predictable from time: Nyuu (alternating-case 24 letters, V8 PRNG needed; yEnc-header Opus thread has a tested solver), postie default (crypto/rand), ngPost (QUuid v4). postie partial mode: yEnc name = md5(fileName+partNumber), Subject = md5("[f/F] \"name\" - yEnc (p/N)") -> verifiable against a predb candidate name. ngPost/Nyuu resend = new ID ("ghost" first attempts may exist); postie/nzbrefresh keep it.

Probe mechanics:
- STAT <id>: 223/430, group-independent, pipelinable (RFC 3977). But Nyuu notes some servers need a GROUP first (config.js L72).
- Latency-bound: nntppool StatInflight 100 with 20 conns, ExistsMany concurrency 64; SABnzbd pre-check STAT, falls back to HEAD on 500, pipeline 1-20 (default 2). Estimate thousands/s per provider.
- No published provider policy on mass STAT/430: throttle per provider.
- Negative caching exists (mjc/nntp-proxy caches 430 per id+provider): don't probe fills too early (Nyuu waits 5 s then 30 s).
- Missing only when every provider 430s (nzbrefresh, nzbcyclops github.com/Appz4Fun/cyclops). nntppool skips providers whose retention can't cover the date.
Prior art: NxG Loader (no NZB), nzbsearcher NZBLNK directsearch (group + date header scan), nzbCheck, Rust nzb-nntp crate. No tool predicts timestamp-format IDs.
Next: fingerprint classifier with real-vs-imitation tells; GPU/SIMD NxG seed search; real PowerPost/JBinUp headers; measure STAT/s and GROUP requirement on 2-3 backbones.
