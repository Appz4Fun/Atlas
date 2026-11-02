# W7A non-video packaging conventions (Haiku, ~09:55Z)
- MP3 scene: .sfv + .nfo + .m3u compulsory by 1999; restricted charset; Artist-Album dirs | scene origin | free from name list. https://www.mp3scene.info/articles/6/mp3-release-standards-1996-to-1999/
- EAC "==== Log checksum ... ====" line and XLD "-----BEGIN XLD SIGNATURE-----" block: hash over the whole log text, so the log (track CRCs, AccurateRip/CTDB lines, offsets) is a per-rip fingerprint | same rip | 1 article. https://tools.rus.io/log-checksum-checker/
- Scene 0day/ebook: NFO + FILE_ID.DIZ in every archive, volumes BASENAMEXX.zip/rar 01-99, 2-4 char group prefix. http://www.roysac.com/blog/2009/03/how-to-package-a-scene-release/
- FitGirl repacks: setup.exe, FG-01..FG-XX.bin (+ FG-Optional-*.bin), MD5/fitgirl-bins.md5 + QuickSFV.exe, "Verify BIN Files Before Installation.bat" | specific repacker + exact build (md5 list = per-file hashes) | free names, 1 article for the md5 list. https://dtptips.com/understanding-fitgirl-repack-files-essential-vs-optional/
- ComicInfo.xml (Series/Number/Volume/credits/Pages) | same work, not same file. kcc wiki ComicRack-metadata
- Unverified: FLAC/ebook-specific scene rules, DODI/GOG layouts, anime [CRC32] conventions, comic scanner pages.
