# W5A PAR2 defaults per tool (Haiku, ~08:55Z)
- par2cmdline: no -b/-s -> blocksize = ceil(total/2000), multiple of 4 (raised to keep <32768 blocks); -r 5%; first exponent -f 0 (exp-0 is in the first vol file); vol naming volX+Y. So identical file sets re-PAR'd with defaults get the same slice size -> same exp-0. Adding/removing a file changes slice size. src/commandline.cpp; manpages.opensuse.org par2create.1
- ParPar: -s REQUIRED (no default); -S auto caps at 32768 slices; slice multiple 4 B; -r default 0; --recovery-offset default 0; pow2 distribution, first file 1 slice. help-full.txt (jsdelivr @animetosho/parpar@0.4.5)
- ngPost: PAR2 off unless --par2_pct; par2cmdline is the default embedded generator; article size default 716800; default PAR2_ARGS unverified.
- Nyuu: no PAR2 creation (planned ParPar integration only). postie/MultiPar/QuickPar/NewsUP/JBinUp: unverified (MultiPar equivalent per ParPar help: -s0.5w*10).
Comparable exp-0: par2cmdline-default vs par2cmdline-default with same file set (strong, common); ParPar vs ParPar with same -s; cross-tool only if slice sizes coincide.
