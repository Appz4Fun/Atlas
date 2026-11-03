# Wave 2 / Agent C: NZB generator fingerprints (Haiku). Mostly unverified.
- Spec (sabnzbd.org/wiki/extra/nzb-spec): iso-8859-1 decl + newzBin DTD 1.1 DOCTYPE + xmlns newzbin 2003. A different decl, or none, is a fingerprint.
- Meta types: title, password, tag, category; custom types use an X- prefix. The same password meta across NZBs points to the same uploader.
- <file poster= date= subject=>: poster copies From, date is the unix server time, subject counter is rewritten to (1/yy). segment bytes= is the article size (encoded vs raw is unstated).
- Nyuu: --nzb-title/tag/category/password, --nzb-poster (default From), --nzb-subject (default first article Subject), --date now pins timestamps, --minify, --nzb-compress, --nzb-encoding utf-8 (so NOT iso-8859-1). src raw.githubusercontent.com/animetosho/Nyuu/master/help-full.txt
- ngPost: --meta, __nzbPoster__, GEN_FROM; with obfuscate=article the article From is random, so the NZB poster is the only stable identity.
- newsmangler/GoPostStuff/JBinUp/NewsUP writers and newznab:attr: not read.
