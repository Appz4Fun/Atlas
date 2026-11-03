# Wave 1 B: multi-group spray and collection grouping (Haiku, ~9 searches)
- ngPost GROUP_POLICY: all groups / one group per post / one group per FILE. Under per-file, one release scatters across groups, so grouping must not key on group. https://github.com/mbruel/ngPost (med)
- ngPost has no built-in random group choice; -g takes an explicit list. Random per-file groups come from poster wrapper scripts, detect from data. (low-med)
- PAR2 per-file MD5 as cross-group join key; r/newznab proposal to store file hashes. reddit mirror of r/newznab/comments/1dfuqi (med)
- Subject + total-parts completeness grouping (nntpit ReleasesExtractor); no time window or multi-group logic. https://sourceforge.net/p/nntpit/wiki/ReleasesExtractor/ (med)
- Crosspost = same Message-ID, Xref lists group:num pairs; separate posts have distinct Message-IDs even with same subject. (high principle)
- [unsourced] Date delta <30 min + article-number burst proximity + same From across groups. (low)
- yEnc (name,size) constant across all parts and groups = file identity. (high)
Follow-ups: ngPost GROUP_POLICY code; nZEDb/newznab-tmux collection regex + time window; nntpit indexing page.
