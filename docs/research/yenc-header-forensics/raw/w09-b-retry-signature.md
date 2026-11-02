# W09-B: retry signature in public NZBs (same file, same segment, two Message-IDs)

Scope: 375 unique public GitHub NZB paths from server-ids/list.txt (1989 rows, 375 unique .nzb).
Fetched 364 (11 404 on both pinned SHA and branch). Parsed 332, skipped 15 unparseable/empty,
17 example-host fixtures (also skipped 2 with bogus segment numbers > 1e6, counted separately).
Caveat: pinned-SHA raw URLs returned 404 in this sandbox (even for known paths), so 364 of the files
were fetched from the repo's master (or main) branch, not the pinned commit. Content may differ from the listed SHA.

Results (parsed NZBs, 3029 files):
- NZBs with any duplicate segment number: 4 of 332 (0188, 0204, 0221, 0327).
- Files with duplicates: 4. Duplicate groups: 4.
- Duplicate groups with identical bytes=: 2. With differing bytes=: 2.
- Message-ID classes in duplicate groups: 7 other, 1 nyuu (pairs: other+other 3, nyuu+other 1).
- Gaps (missing segment numbers below max): 7 files, 3796 missing numbers in total (not broken down further).

Examples:
- 0204 (sanderjo/NZBs reftest, 666MB fixture): segment 1 twice, IDs
  DoLbJjHmPbBbPoVvZsCfEpMv-1501275130640@nyuu and a non-nyuu junk ID; bytes 728903 vs 728916. Not a retry: sizes differ, the second ID is not in Nyuu form.
- 0188 (watzon/caravan testdata): segment 1, dup.1@news.example.invalid and dup.2@news.example.invalid, bytes 1024 both. Test fixture, example host.
- 0221 (junkerderprovinz/knightloader testdata): segment 2, part1-2@example.invalid and part1-2-repost@example.invalid, bytes 768000 both. Fixture, labeled "repost".
- 0327 (scryer-media/weaver fuzz seed): segment 1, "same-id" and "duplicate-number". Fuzz fixture.

Interpretation:
- The only real-looking duplicate is 0204, and its bytes differ. No group shows a Nyuu/ngPost pair on one segment with matching bytes, so this sample gives no evidence of a retry signature (same bytes, close ms timestamps, one Nyuu and one ngPost ID).
- Every duplicate in this sample comes from a fixture or test file. The sample is too small and too test-heavy to say how common retries are in the wild.
- Writers: 110 parsed NZBs have a nyuu marker in the first 4000 chars, 2 have ngPost. 220 are unknown. None of the Nyuu or ngPost files contains a duplicate segment, so no writer is shown to keep both entries.

Not done in this run:
- SABnzbd and NZBGet parser source check (not cloned: github.com returns 403 from this sandbox). Their behavior for a duplicate segment number is unverified here.
- Per-file time comparison for Nyuu timestamps (only one Nyuu ID appears in a duplicate group).
