# Wave 1C: indexers and researchers (Opus subagent)
- NNTmux collectionhash = sha1(cleanedSubject . totalFiles); enhanceShortSubject() appends punctuation skeleton of short/random subjects (template fingerprint). Xref parsed so crossposts share one collection. (NNTmux/newznab-tmux app/Services/Binaries/CollectionHandler.php, CollectionsCleaningService.php, XrefService.php)
- NNTmux NameFixingService "donor" joins: RAR/7z/zip inner CRC32 (5% size tol), PAR2 hash_16K (5%), MediaInfo Unique ID / MKV SegmentUID (10%). DonorMatchSelector.php, Par2Processor.php.
- nzb_guid = md5(first segment Message-ID), dropped 2026-05.
- PAR2 RSID = MD5(main packet body); File ID = MD5(hash16k||len||name). SABnzbd par2file.py combines IFSC slice CRCs into full CRC, drops hash16k that maps to two names.
- Nyuu defaults: msgid rand24-ts@nyuu, From local user@host, line 128, article 700K, yenc name= defaults to filename even with custom subject.
- NxG Upper: msgid = SHA256("HEADER:data|par2:N") as 40hex@21hex.3hex — detectable shape; confirm set from header. https://github.com/Tensai75/nxg-upper
- Bijnen 2015 OS3 study: XOVER crawl 3.2M filesets; one article availability predicts fileset. https://rp.os3.nl/2014-2015/p82/report.pdf
- Rights holders work from leaked NZBs, not raw headers.
Leads: PAR2 slice vs yEnc part boundary alignment (ParPar defaults); posting-tool template catalog + Xref article-number contiguity; external donor tables (animetosho CRC/MD5/ED2K, predb CRC, nZEDb predb_hashes).
