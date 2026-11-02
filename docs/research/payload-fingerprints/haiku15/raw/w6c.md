# W6C clustering / record linkage (Haiku)
- Fellegi-Sunter: weight log2(m/u) per evidence (splink docs, robinlinacre.com).
- Papadakis 2020 blocking survey: size+CRC32 is a blocking key, not evidence.
- Transitive closure chains; Hassanzadeh VLDB 2009 (CENTER single-pass); correlation clustering Pivot (3-approx), streaming variant arXiv 2305.13560; Rust crate clump (PIVOT + union-find).
- Splink: threshold components + bridge detection / cluster density to flag weak joins.
- Eval: pairwise P/R per evidence tier, closest-cluster F1, variation of information; gold set few hundred-few thousand pairs; alarms on largest cluster size and bridges.
- Policy: LINK = full MD5+len (and per-creation random field equality); CANDIDATE = size+CRC32 until 2nd signal; ANNOTATE = tool/uploader/size hints, never edges. Union-find over link edges only. Fits the owner's rule: group twins, never collapse rows.
