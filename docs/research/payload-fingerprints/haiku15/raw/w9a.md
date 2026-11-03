# W9A PAR2 index presence (first pass, 10:24Z)
- ParPar writes an index file by default (--noindex turns it off). Spec: only Creator is mandatory per file; Main/FileDesc/IFSC "should" be in every volume. Not yet confirmed from tool source that every volume carries them.
- Obfuscation wrappers (upapasta --obfuscate) rename the .par2 files too; NewsBin groups sets by PAR internals not names. par2-rs rename.rs matches on hash_16k.
- My correction: Recovery Set ID is NOT name-independent (File ID = MD5(hash16k||len||name)), so key on full MD5 + length (+ hash16k), not set ID.
- No published share of posts carrying PAR2. QuickPar has "restrict block size to multiples of article size"; ParPar has --slice-size-multiple; no tool defaults slice = article size (unconfirmed).

## Second pass (10:30Z)
- par2cmdline par2creator.cpp: one critical-packet set (Main+FileDesc+IFSC) in the index file; recovery volumes get critical packets interleaved, more copies in bigger volumes (full set per volume inferred, not verified). Creator once per file.
- ngPost: PAR2 OFF by default (--par2_pct 0), embedded par2cmdline, article size 716800; -x obfuscates subjects, --gen_name random RAR names; no PAR2 rename option.
- Nyuu README example: outputIndex true, sliceSizeMultiple 4 shown as defaults. No tool ties slice size to article size.
