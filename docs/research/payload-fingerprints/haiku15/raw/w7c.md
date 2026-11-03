# W7C 7z / ZIP stored-mode (Haiku), from 7-Zip HEAD source
- 7z: header compressed (kEncodedHeader, LZMA) by default even at -mx0 when >=2 items (hc=on); for 1 item header uncompressed? (compressMainHeader false when numItems<2).
- 7z defaults: kMTime yes, kAttributes yes, kCTime/kATime no unless -mtc/-mta. Per-file CRC in SubStreamsInfo (or UnpackInfo for single-file folder). -mx0 packed data contiguous from offset 32; header at tail -> last article.
- ZIP: Go writer always bit 3 (data descriptor); Python bit 3 only if unseekable; Java STORED requires CRC upfront (no DD). Central directory at end is authoritative -> last article.
- Info-ZIP 3.0 TESTED: zip -0 (file and stdout) writes flag 0x0 with real CRC32+sizes in the LOCAL header -> inner CRC+size from article 1 for Info-ZIP stored zips. Go always uses data descriptor (CD at tail).
