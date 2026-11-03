# W09-B: same-film keyframe hash, PQ and unrelated-scene control (COMPLETE)

Code: code/w09b_main.rs (usage: w09b <workdir>, expects var/*.264, var/*.265, var/ref_*.yuv). Decoder path verified: decoded luma matches ffmpeg 100% on all 7 variants.

## Method
- Source: Big Buck Bunny 1080p, 24 fps (npm big-buck-bunny-1080p, as in w08-b). Each variant is 10 s.
- a x264 crf18 1080p (-ss 5). b x265 crf26 720p 10-bit. c 2.39:1 letterbox in 1080p. d brightness +0.04 / gamma 1.08. f 25 fps retime (same pixels, new timestamps).
- NEW e: x265 10-bit PQ, zscale bt709 -> smpte2084/bt2020, hdr10=1. No tone curve, so this is a weak HDR test.
- NEW g: unrelated scene, x264 crf18 from -ss 20 (same film).
- Decoding: sequential rust_h264 0.6.0 / rust_h265 0.4.0, display order, all 240 frames. Five test positions are display indices 12, 60, 120, 180, 228 (0.5, 2.5, 5, 7.5, 9.5 s).
- Preprocessing: none; crop (drop edge rows/cols with mean luma < 20, 8-bit scale); eq (256-bin histogram equalisation); crop+eq. 10-bit luma is shifted to 8-bit first.
- Hashes: dHash 8x8 (9x8 box resize); pHash 32->8 DCT median (64 bits); blockhash16 (image_hasher, 256 bits). Threshold 10 for 64-bit, 40 for 256-bit.

## Distance to (a), bits, mean over 5 positions / max
| variant, prep | dHash8 | pHash8 | blk16 | pHash pass-of-5 |
|---|---|---|---|---|
| c none | 14.8 / 22 | 19.2 / 22 | 80.8 / 86 | 0 |
| c crop | 0.0 / 0 | 0.0 / 0 | 1.2 / 4 | 5 |
| c eq | 14.0 / 19 | 18.4 / 22 | 82.4 / 88 | 1 |
| c crop+eq | 0.0 / 0 | 0.0 / 0 | 0.4 / 2 | 5 |
| d (any prep) | 0.0-0.4 / 0-1 | 0.0-0.4 / 0-2 | 0.0-1.0 / 0-1 | 5 |
| f (any prep) | 0 / 0 | 0 / 0 | 0 / 0 | 5 |
| b none/crop | 0.2 / 1 | 0.0 / 0 | 6.8 / 12 | 5 |
| b eq/crop+eq | 0.4 / 2 | 0.8 / 2 | 2.0 / 4 | 5 |
| e none/crop | 1.4 / 2 | 2.0 / 4 | 10.0 / 10 | 5 |
| e eq/crop+eq | 1.2 / 4 | 2.0 / 4 | 1.6 / 3 | 5 |
| g none/crop (diff scene) | 33.8 / 36 | 32.0 / 34 | 121.2 / 159 | 0 |
| g eq/crop+eq (diff scene) | 33.2 / 35 | 30.0 / 32 | 120.6 / 162 | 0 |

## Separation
- a vs g (diff scene, same film), pHash min over 5 positions: 30 (none), 24 (crop+eq). dHash min 30 (none). blk16 min 87-91.
- Within a (10 pairs of positions), pHash min 2 (none, mean 27.0) and 2 (crop+eq, mean 26.2). One pair is at or under threshold 10 for dHash, pHash and blk16 (fp 1/10). The cause was not checked; likely near-flat frames.
- Same-version distance max is 0-4 for pHash, against a different-scene minimum of 24-30.

## Recommendation
- Hash: pHash8 (DCT, 64-bit) on crop+eq luma. Crop is required for letterboxed releases (c: 19.2 -> 0). Eq is cheap insurance and did not hurt any case. blockhash16 adds no margin here.
- Threshold: Hamming <= 10/64 per frame. Observed same-version max 4 and different-scene min 24.
- Vote: take 3 frames (positions 0.5 s, 5 s, 9.5 s). Accept a match when at least 2 of 3 pass. A single frame gives 5/5 same-version passes but also collides on one within-film pair, so a 2-of-3 vote is advised. This vote was not measured against a collision case.
- PQ: e already passes with no preprocessing (pHash 2.0). Crop+eq does not change that. This does not prove HDR10 vs SDR grades match, because the PQ re-encode is a plain transfer change with no grade. Test on a real HDR grade before relying on it.

## Limits
- One film, one 10 s window per variant. g is a different shot from the same film, not a different film.
- f is pixel-identical to a, so it only tests timing invariance.
- Five positions, not a full keyframe scan.
