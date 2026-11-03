# Wave 6 merged (09:25-09:32Z)
1. Highwinds IDs, on 5 posts: K is constant for the whole post and distinct between posts (a session key, not an account). T1 = injection time to the second (epoch 876645500, base-64 with 64 symbols incl. '_' and '%'). N = a front-end counter with 0-27% other-user interleave. R = random. Key = (host, K, T1 window, N range).
2. Server lineage: INN uses the same idea (radix32(time-673416000)$radix32(pid)$count). Highwinds is a base-64 variant with a 1997-10-12 epoch. Diablo is the likely ancestor (not verified, since source is proxy-blocked).
3. From other threads: X-Received-Body-CRC is SOLVED (yEnc 15-min thread) as zlib.crc32(body incl. CRLF + ".\r\n"), so a HEAD/HDR-only byte-twin key for the same tool and split. Highwinds Path hex after POSTED! is per-feed, not per-account.
4. Version marker: node-yencode #3 (Oct 2016 Nyuu) left critical chars unescaped at line end.
5. New suffixes seen: @UFK (likely Usenet.Farm), @PRiVATE.
