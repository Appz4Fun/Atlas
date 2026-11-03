# Wave 4 / Agent A: decoding server-assigned Message-IDs (Haiku). Nothing found.
- No public source decodes the Highwinds `NzZUA.117908$rY1.16813@fx45.am4` format. Provider KBs (Newshosting 1653, Easynews 1655, Tweaknews 1801) obfuscate their examples. RFC 5536 3.1.3: the local part has no required structure.
- Searches for @fx45.am4 / @fx14.ams / @fx01.iad / $rY1 / hex$0$ found no samples.
- INFERRED only from 2 samples: 117908->117909 looks like a per-process article counter.
- Known from another thread (posting-behavior Opus, wave 3): INN generates `<radix32(time-673416000)$radix32(pid)$count@domain>` (inn lib/messageid.c), with one nnrpd pid per connection, which gives time, connection and counter.
Best next step: sample real XOVER from prod (Atlas already stores Message-IDs) and grep for `\$.*@fx\d+\.` to get hundreds of samples, then decode by sorting on article number/date.
