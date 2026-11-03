# Wave 5 A: header-to-NZB grouping prior art (Haiku; cloned source)
| tool | grouping key | window | multi-group | completeness |
|---|---|---|---|---|
| nzb-monkey-go directsearch (Tensai75/nzbmonkey-go + nntpDirectSearch, subjectparser) | headerHash=MD5(subject.Header+poster+TotalFiles); fileHash=MD5(headerHash+Filename+File+TotalSegments). Match = case-insensitive substring of h in Subject | 12h back/12h fwd around d; date-driven boundary search (Tolerance 30s, Step 20000) | per group, no merge; FirstGroupOnly option | MaxMissingFiles / MaxMissingSegmentsPercent thresholds |
| NNTmux (app/Services/Binaries) | collectionHash=sha1(regexName+totalFiles); file key md5('file:'+n) else md5(subject+poster) | not verified | stores Xref groups per collection -> one collection can span groups ONLY if Xref lists them (true crossposts) | - |
| nZEDb (Binaries.php) | md5(subjectPrefix + From + groupId) | - | group id in key -> no merge | regex [n/m] |
| pynab (binaries.py) | hash(name, group, poster, total_parts) | - | group in key -> no merge | - |
Breakage: (a) ngPost group-per-file: every tool splits it (group in key or per-group scan); NNTmux merges only true crossposts. (b) Postie full obfuscation: all key on Subject+From, none read yEnc; every article becomes its own binary. -> Atlas needs a body-level or behavior-level key; no prior art handles this.
