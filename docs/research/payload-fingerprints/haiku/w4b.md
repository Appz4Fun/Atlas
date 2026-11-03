# W4B key strength / collision stats (Haiku, ~08:25Z)
- No published rate of exact-size collisions for large media; gap -> measure on Atlas's own catalog (size histogram, same-size pairs with different CRC). Agrawal FAST'07 only gives distributions. microsoft fiveyearstudy.pdf
- CRC32 birthday 50% at 77,163 items; forgeable by 4 chosen bytes (nayuki forcing-a-files-crc). Never a key alone; size+CRC32 = strong candidate (collision space is per-size-bucket).
- MD5-16k is a prefix hash; shared container headers can collide (inference, no documented case). OpenSubtitles moviehash = size + sums of head/tail 64 KiB, page itself warns of collisions and forgeability. https://opensubtitles.github.io/oshash/
- Dedupe tools: size -> partial (czkawka prehash first+last 4 KB) -> full hash -> byte compare.
- eD2k MD4 collision attacks (exploit-db 13689): one weak hash isn't identity under adversaries.
Ladder (my edit of agent's): 1 CRC32 alone: never. 2 size alone: candidate. 3 size+hash16k / moviehash: candidate. 4 size+CRC32: strong candidate. 5 size+CRC32+hash16k (+pcrc32 interval matches): practical alt. 6 size+full MD5 (PAR2 FileDesc) ideally + CRC32: twin for non-adversarial use. Byte compare is the only proof but impractical; use 6.
