# Wave 1 A: uploader tool fingerprints (Haiku, ~10 calls)
Source for Nyuu items: https://raw.githubusercontent.com/animetosho/Nyuu/HEAD/help-full.txt ; ngPost: https://github.com/mbruel/ngPost
1. Nyuu default Message-ID ^[A-Za-z0-9]{24}-\d+@nyuu$ ; numeric timestamp field may be shared per run (med)
2. Nyuu --date now: all articles of a run share one Date header (med)
3. Nyuu default subject template {comment} [{0filenum}/{files}] - "{filename}" yEnc ({part}/{parts}) {filesize} {comment2}; post key = (files, filesize set) (high)
4. =ybegin name constant per file; mismatch vs subject filename flags obfuscation (high)
5. Uniform decoded part length from =ypart begin/end (Nyuu 700K, ngPost 716800); ceil(size/len)==total (high rule)
6. yEnc line length (Nyuu default 128) constant per run (med)
7. ngPost -x: Subject == Message-ID local part (UUID), MID host default "ngPost"; retries get new UUID (med)
8. ngPost random From by default, --gen_from per post; From only usable when fixed (med)
9. Nyuu -H custom headers with ${rand(N)} per-run token (low)
10. Newsgroups string format (comma, no spaces) per tool (med)
11. Powerpost fixed segment sizes, last = remainder. https://www.harley.com/usenet/file-sharing/14-the-process-of-posting-a-file.html (med)
Unverified: posting order, X-Newsposter/User-Agent.
Unopened leads: gist retroplasma/264d9fed2350feb19f977575981bb914, github Tensai75/nxg-upper, estalker82/ngPost, gist dbiesecke/f5aee99d011aec62e8bb8e57bf83f479
