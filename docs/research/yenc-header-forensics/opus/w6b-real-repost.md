# Wave 6B: real repost test + header noise (Opus subagent)
Dataset 229 NZBs / 6,029 files (Nyuu 5,784; UFK 92; ngPost 54; legacy few). Scripts: repost-test/ (load.py, cmp.py, cmp.txt).
- PAIR A (REAL REPOST CONFIRMED): sanderjo "unique_files_with_subdir_ee0fb..." (t=1613742799) vs "..._in_su..." (t=1613812479), ~19.4 h apart, all different Message-IDs; myfile1-3, insub1-2 (10 MB, 15 segs) match 15/15 segment bytes exactly; high-entropy (sd~100) -> chance ~0.0028^14. PAR2s differ (content changed).
- Pair B: re-RAR ~2 min apart (rwx_all_on_777 vs 100MB_r__400): parts 02-09 15/15 identical, part01 13/15 (seg1 -1), part10 7/8; low-entropy content so weak evidence. PAR2 recovery volumes unrelated.
- Pair C (control): same name, regenerated random content -> unrelated (diff ~-11,500 ±100). Key correctly does not link.
- Pair D: low-entropy variants differ by constant 121/segment (likely different fill pattern).
- ngPost NZB bytes = 716800 flat (decoded size) -> no escape info; writer fingerprint; gate out.
- UFK NZB (ubuntu-mate ISO): full segs ~793-794k, P=768000 inferred, ~950 B headers included; high-entropy within-file sd 96-132 (theory ~109) -> header length near-constant per post; per-file offset should rescue key on header-inclusive sizes (inference, not verified with a same-content pair).
- No different-tool same-content pair found.
Leads: same post in body-only + header-inclusive NZB; entropy gate + ngPost gate; byte-range (not segment-index) matching for different article sizes.
