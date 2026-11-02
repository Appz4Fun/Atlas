# Wave 6 / Agent A: Highwinds ID stats on 5 public posts (Haiku, computed)
| source | host | arts | K | N range | gap | T1 span |
|---|---|---|---|---|---|---|
| jwpaine/NZBFetch earth_wallpaper.nzb | fx02.iad | 1 | 6aQa | 8588 | - | 2025-10-01 10:20:13 |
| megadrive/premiumize-api-issue Big.Buck.Bunny.nzb | fx33.iad | 655 (635 parsed) | 8E2 | 251076-251937 | 26.3% | 2016-12-06 04:00:26-04:08:47 (1.27/s) |
| tumblfeed/nzbunity test/test.nzb | fx30.am4 | 180 | EN | 288222-288406 | 2.7% | 2016-11-04 15:59:01-16:03:29 |
| sabnzbd.github.io tests/basic_rar5.nzb | fx31.am4 | 8 | 4Z1 | 55279-55289 | 27.3% | 2017-05-23 16:37:12-18 |
| sabnzbd.github.io tests/test_zip.nzb | fx45.am4 | 11 | rY1 | 117908-117918 | 0% | 2017-05-23 16:38:05-12 |
- K is constant for the whole post (no change over 501 s), distinct across all 5 posts, and not monotone in time. It is 3 chars in 2016-17 and 4 chars in 2025. Two posts 1 minute apart on different am4 front ends had different K. INFERRED: K is a per-session/connection id, not an account id.
- The T1 alphabet has 64 symbols: 0-9a-zA-Z plus '_' and '%' (seen in 20 BigBuck IDs; their order as 62/63 is not known).
- R is uniform over ~0-290k, with no structure.
- N interleave is 0-27%.
- Other formats seen: `<rand>-<ms>@PRiVATE` (nzbget-ng samples), `<rand>@UFK` (ubuntu-mate-16.04.2; UFK = Usenet.Farm? cf. X-Ufk), `<rand>@NNNNNNN`, a dotted random right side (`...@hL_ArB.lfoRvh.mMOe.siBGJt`), and lowercase `vbkretgqnq@zor2h64.1pb6`.
