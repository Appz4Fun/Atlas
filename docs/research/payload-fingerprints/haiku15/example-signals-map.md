Atlas research · 2 Nov 2026

# Linking signals map

Every signal the research threads found for tying Usenet posts together, sorted by what it proves (twin, alt, same film, same uploader, posting tool) and by how much has to be downloaded to read it. The colour shows whether Atlas collects it today.

## What each signal proves, and what it costs to read

One dot per signal. Rows go from free (already in the overview row) down to fetching every article. Hover or tab to a dot for details. Most of the cheap twin and alt keys are still uncollected.

Sources: `research/linking-findings.md` and each topic's `log.md`. Atlas status from the catalog thread's check against main at a29ee52 and the archive and stream threads' probe tests. Grey means the research did not check Atlas for that signal.

## Atlas coverage by download cost

Signals per cost tier, split by what Atlas does with them now.

## Articles to fetch for media keys

Frame and audio fingerprints need mid-file reads from a store-mode RAR, and audio is the expensive one because it is interleaved with video.

Source: stream-metadata wave 4 sampling plan. Tail is "3 to 5", drawn at 5.

## Default segment size by posting tool

Nyuu and ngPost both default to 716,800 bytes, so size alone can't tell them apart. The Message-ID shape can.

Source: posting-behavior waves 1 and 2. The axis starts at 700,000 bytes to show the gaps; it is a dot plot, not bars.

## Posting tool fingerprints

What each tool leaves in headers by default. These classify a post's tool even when names and subjects are obfuscated.

| Tool | Segment size | Message-ID shape | Other tells |
| --- | --- | --- | --- |
| Nyuu | 716,800 | `rand24-{unix ms}@nyuu` | Date is article build time, monotone in queue order. Default From leaks the local user@host. Post-check repost keeps the same Message-ID, 35 s or more later. |
| ngPost | 716,800 | `<uuid v4>@ngPost` | Sends no Date, so the server sets it. "One group per file" policy splits a post across groups. A retry gets a new UUID. pcrc32 is printed without zero padding. |
| Postie | 750,000 | `32@8.3` random | Defaults to one group per file and full obfuscation of names, subjects, dates and poster. Reposts a missing article after 10 s to 5 min. |
| NewsUP | 768,000 | not recorded | Fixed From `NewsUP <NewsUP@somewhere.cbr>`. |
| GoPostStuff | not recorded | `UnixNano$gps@gopoststuff` | pcrc32 in upper-case hex. Header order From, Newsgroups, Message-ID, X-Newsposter, Subject. |
| nxg-upper | not recorded | `40hex@21hex.3hex` | Message-ID is SHA-256 of the NxG header, so every data and PAR2 ID can be derived from it. |

## Tested in pure Rust

Results from the stream thread's runs on generated samples, which back the "same film" keys without ffmpeg.

Chromaprint score, FLAC vs AAC 128k

0.992

Unrelated audio scores 0

Frame hash distance, 1080p to 360p

0 to 3 of 64 bits

Identical across 8 and 10-bit, 720p and 1080p

HEVC keyframe decode

\~10 ms

rust_h265, bit-exact against ffmpeg

AC-3 crc2 sequence across remux

Identical

Same audio in MKV and MP4

## Signal inventory

The table behind the charts. Filter by Atlas status.

| Signal | Proves | Atlas | Notes |
| --- | --- | --- | --- |