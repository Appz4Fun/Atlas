# Wave 4 merged (08:24-08:28Z)
- PAR2 bridge works with every tool checked: decode the first article of each slice, md5 its first 16 KB, and join (size, hash16k) to the PAR2 index FileDesc packets; confirm by recomputing the set ID. Name is never a safe key.
- Postie (default full obfuscation) gives each ARTICLE its own random yEnc name, so even within one file the name changes. Wave 1's "yEnc name constant per file" is false for Postie. Link Postie articles by size= + begin/end contiguity + pcrc32 instead.
- ngPost renames before RAR, so PAR2 names = yEnc names; Postie builds PAR2 on real names. Creator strings: "ParPar v<ver> <arch>", "Postie".
- Spotnet gives verified ground truth: spot (free.pt, RSA-signed X-XML) -> NZB article Message-ID (alt.binaries.ftd) -> full segment list for one post.
- NZBLNK + nzb-monkey-go "directsearch" already build NZBs from headers in groups g around date d: client-side prior art for header linking.
- Same-uploader linking over months: Timeprints (cosine on hour/period/weekday histograms, ~500 posts for >80%) and gap-distribution KS (>=20 events). Use server arrival time, not Date (Postie jitters Date 0-6h).
