# W7B PAR2 index layout (Haiku, from source)
- par2cmdline index (.par2, 0 recovery): one copy each, sorted: Main | FileDesc xN (by FileId) | IFSC xN (by FileId) | [UniFileN] | Creator "Created by par2cmdline version X."
- ParPar index: FileDesc0|IFSC0|FileDesc1|IFSC1|...|Main|Creator "ParPar (library) vX [https://github.com/animetosho/parpar]"; IFSC skipped for empty files. Packet order itself fingerprints the tool.
- Sizes: Main 76+16N; FileDesc 120+ceil4(name); IFSC 80+20S; Creator 64+ceil4(len).
- 4 GiB file: par2cmdline default S=2000 -> index ~40.4 KB; ParPar -s1M S=4096 -> ~82.4 KB. Cap 32768 slices -> IFSC <=655,360 B -> whole index fits one 700 KB article for up to ~200 files.
- Critical packets repeated only in recovery volumes.
=> Cost tier for PAR2 FileDesc + IFSC: ONE article (the index file's first/only article) in nearly all posts.
