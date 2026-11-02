# Wave 7 (09:56Z to 10:05Z): classifier run, consolidated table, backbone quirks

## What we did
- A: ran a Message-ID classifier (tools/msgid-classifier/) over the shared corpus posting-behavior/server-ids/segs.tsv (72,334 segments, 246 NZBs).
- B: merged waves 1 to 6 into one Atlas-ready table: fingerprints.md (tools, server families, conflicts C1 to C32).
- C: backbone quirks (Usenet.Farm, Highwinds headers).

## Results
1. Classifier coverage. Known grammars cover about 86% of segments. nyuu 23,831 segs / 103 NZBs; -13digits@PRiVATE 11,628 / 5; ngPost 3,689 / 2; yEncBin "Part N of M.HEX32@ts.local" 2,816 / 4 (new grammar); Usenet.Farm @UFK 2,775 / 2; PowerPost 1,572 / 2; Diablo 1,218 / 10; newsmangler 895 / 5; Highwinds 869 / 7. Unknown 10,370 / 87, dominated by one random-alnum@hex-ish-host family that is not in any spec yet. Zero hits for GoPostStuff, postie, nxg, JBinUp, nntpPoster.
2. Highwinds T1 alphabet resolved (C19). 64 symbols, little-endian, `0-9a-zA-Z` then `_`=62, `%`=63, unix = value + 876645500. 0 time decreases over 862 pairs; the reverse order gives 10.
3. Highwinds $K is one value per NZB (7 distinct over 7 NZBs). Agent A's "K" stats actually describe the random R field; corrected in raw/w07-a-classifier-summary.md. N gaps per NZB range 0% to 43%.
4. Usenet.Farm headers verified from a real capture: X-Ufhash, Date rewritten as 2-digit year + literal "UTC", Path not-for-mail, `Message-Id` casing, X-Received-Bytes and X-Received-Body-CRC. The client Message-ID is kept. One @UFK NZB was posted with From "NewsUP <NewsUP@somewhere.cbr>".
5. Highwinds injection: Path ends `fxNN.<site>.POSTED!not-for-mail`, numeric-offset Date with 4-digit year, X-Received-* present.

## What it means for Atlas
- Normalise Date to epoch before using its form as a tool fingerprint: a Farm rewrite destroys the poster's Date form.
- The classifier is ready to port: Message-ID grammar gives the posting tool for about 86% of segments from overview data alone.
- Highwinds (host, K, T1 window, N range) is a cheap same-session key from overview data.

## Still open
postie part size (750000 or 768000), whether postie and Nyuu retries keep the Message-ID, the unknown random-alnum family, Highwinds XOVER Lines, X-Ufk meaning.

## Next wave (8)
Lean in: (a) identify the big unknown Message-ID family, (b) settle postie size and retry behaviour from source, (c) write a Rust-ready spec for the classifier and session keys.
