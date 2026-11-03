`code.bin` is 16 KiB of synthetic "code": random bytes with branch and call patterns for x86, PowerPC, IA-64,
ARM, ARM-Thumb, SPARC and ARM64 mixed in. Each `.xz` is that file packed by liblzma with one filter (or Delta and
x86) in front of LZMA2. `arm64.xz` came from the xz 5.4 command line, and the rest from Python's `lzma` module.
