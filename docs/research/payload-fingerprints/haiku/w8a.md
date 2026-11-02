# w8a: Marker-file sizes and hashes (research pass)

Budget used: 17 WebFetch, about 20 WebSearch. Most marker files could not be verified. Only one marker-named file has a verified size and hash.

| Marker file | Size (bytes) | Hash(es) | Variants | Source URL | Proves | Cost |
|---|---|---|---|---|---|---|
| RARBG.com.txt | 34 (verified in scan report) | SHA256 66f74d305d31b00fd0306d67cbe8bd1730e19ad9c5214dad2a8136d5d582b147; MD5 a7d518b719bf133ff5bd4a0fdf9c8f2f; SHA1 f1d8b4aa6f63a28f7bcb586d139dec1ac6fffbd0 | One variant seen. Content not shown, so the text is UNVERIFIED | https://gridinsoft.com/online-virus-scanner/id/66f74d305d31b00fd0306d67cbe8bd1730e19ad9c5214dad2a8136d5d582b147 | tool / RARBG origin (name match only, not proven to be in RARBG releases) | free (size from yEnc header); CRC32 needs 1 article, and the CRC is not in this source (UNVERIFIED) |
| RARBG_DO_NOT_MIRROR.exe | UNVERIFIED (no size or hash found) | UNVERIFIED | Unknown. Article says it appears in "new video torrents" over a few days, not all torrents | https://torrentfreak.com/rarbg-adds-exe-files-to-torrents-but-no-need-to-panic-190126/ | same uploader / RARBG origin (article confirms purpose, no hash) | UNVERIFIED |
| YIFYStatus.com.txt | UNVERIFIED (diff shows 25 lines, not bytes) | UNVERIFIED | Unknown | https://huggingface.co/colaibu/File-Backup/commit/dc782acc9b4c73417de66246736eb799b2a48b56 | tool / YTS origin (name seen in a 2026 YTS.BZ release folder) | UNVERIFIED |
| www.YTS.MX.jpg | UNVERIFIED (shows +0 -0 only) | UNVERIFIED | Unknown | https://huggingface.co/colaibu/File-Backup/commit/84c8aeb1677ae341a9db219fef8bd2b36201e0cb | alt / same uploader (YTS.MX naming) | UNVERIFIED |
| YTSProxies.com.txt | UNVERIFIED | UNVERIFIED | Unknown | none found | alt | UNVERIFIED |
| TGx / TorrentGalaxy .txt | UNVERIFIED | UNVERIFIED | Unknown | none found | alt / tool | UNVERIFIED |
| ETRG / EZTV .txt | UNVERIFIED | UNVERIFIED | Unknown | none found | alt / tool | UNVERIFIED |
| "Downloaded from" .txt/.url | UNVERIFIED | UNVERIFIED | Unknown | none found | tool | UNVERIFIED |
| ettv / psa / QxR / rarbg sample naming | UNVERIFIED | UNVERIFIED | Unknown | none found | same film (sample name), not a marker | UNVERIFIED |

Non-marker data points (for calibrating how distinctive a size is). These are generic scan-report files, not release markers:

| File | Size (bytes) | Hash | Source |
|---|---|---|---|
| readme.txt | 280 | SHA256 a42121fde8ba3f713e3bae077277b6ad0834ddac1fa70bfb2f5934b13d6c202a | https://gridinsoft.com/online-virus-scanner/id/a42121fde8ba3f713e3bae077277b6ad0834ddac1fa70bfb2f5934b13d6c202a |
| Readme.txt | 226 | SHA256 3f7015a8dc6a3202c9bbd7f1c838edccd429a1a461cddbaae38815c3845459a7 | https://gridinsoft.com/online-virus-scanner/id/3f7015a8dc6a3202c9bbd7f1c838edccd429a1a461cddbaae38815c3845459a7 |
| index.txt | 131 | SHA256 eacc184f7fb81c8163a139b0d60d082dfa9153af18b6201537aa8f0f0f3b21d8 | https://gridinsoft.com/online-virus-scanner/id/eacc184f7fb81c8163a139b0d60d082dfa9153af18b6201537aa8f0f0f3b21d8 |
| information.txt | 92 | SHA256 9482230991095a66219e3c88503e9366b33278b4e1f1337d1749895728f03d86 | https://gridinsoft.com/online-virus-scanner/id/9482230991095a66219e3c88503e9366b33278b4e1f1337d1749895728f03d86 |
| info.txt | 209 | SHA256 4cc7f057471820bb1f01670d51371bc7fc53c9220953b94a02af102637079771 | https://gridinsoft.com/online-virus-scanner/id/4cc7f057471820bb1f01670d51371bc7fc53c9220953b94a02af102637079771 |
| test.txt | 15 | SHA256 92b772380a3f8e27a93e57e6deeca6c01da07f5aadce78bb2fbb20de10a66925 | https://gridinsoft.com/online-virus-scanner/id/92b772380a3f8e27a93e57e6deeca6c01da07f5aadce78bb2fbb20de10a66925 |
| Lilo & Stitch (2025) [2160p] [YTS.MX].torrent | 25,793 | MD5 4a34db863addbcdc1be185d933cce117; SHA256 4892367990a7bbc932e76230e059b81973b52f988348b355b08f0a0846a09904 | https://gridinsoft.com/online-virus-scanner/id/4892367990a7bbc932e76230e059b81973b52f988348b355b08f0a0846a09904 |

Notes on the rows above:
- The `readme.txt`, `Readme.txt`, `index.txt`, `information.txt`, `info.txt` and `test.txt` files are not release markers. They are shown only to show that small text files of many sizes exist.
- The `.torrent` row is a YTS torrent file, not a marker. It is listed only as a size reference.

Variants by version or year: UNVERIFIED. No source compared sizes across YTS or RARBG releases from different years.

Is size alone distinctive? No, not from this data. Generic small .txt files cover 15 to 280 bytes. A 34-byte match on RARBG.com.txt would need the filename or a CRC32 to confirm it.

Sources not fetched or not usable:
- VirusTotal and hybrid-analysis: searches returned no relevant pages.
- GitHub blocklists (Sonarr, Radarr, qBittorrent): searches found only generic filters. The qBittorrent script and a Nyaa userscript have no marker names. The cleanuparr content-blocker doc has only placeholder patterns. The Sonarr forum post has no marker names.
- Hugging Face SplaatKlasky/RARBG: a 26-byte file (website1.web) with no marker.

## Takeaways
- Only one marker-named file has a verified size and hash: RARBG.com.txt, 34 bytes, SHA256 66f74d30... It was seen as a name in a scan report, and its content is unverified.
- RARBG_DO_NOT_MIRROR.exe is documented (TorrentFreak says it is a placeholder against mirroring), but no size or hash was found. A VirusTotal or MalwareBazaar lookup is needed for that.
- Hugging Face commit pages (colaibu/File-Backup) show YIFYStatus.com.txt and www.YTS.MX.jpg, but only line counts in the diff, not byte sizes or hashes. Hugging Face LFS pointers only give sizes for binary files.
- Gridinsoft per-file scan pages (URL id = SHA256) are the most fetchable source. Each gives exact size and MD5, SHA1 and SHA256. Most hits so far are generic readme or info files, so a filename search is needed to find markers.
- Size alone is not distinctive for small text files (15 to 280 bytes in this sample). Pair a size match with the filename, or with a CRC32 from a real article, before treating it as a marker match.
