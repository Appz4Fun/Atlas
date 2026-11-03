# w9b: srrDB as an oracle for naming obfuscated posts

Scope: srrDB API keywords, response shapes, rate/terms, SRR format, other scene CRC/size sources. Fetch budget ~20 WebFetch. Items marked UNVERIFIED were not confirmed against a live response (srrdb.com/api/ is robots-disallowed to the fetcher, so no live JSON was seen).

## Lookup table

| Lookup | Endpoint/query | Input from post | Returns | Proves | Cost | Source URL |
|---|---|---|---|---|---|---|
| Inner-file CRC32 to scene release (the key one) | `GET https://www.srrdb.com/api/search/archive-crc:<CRC32 hex>` (keyword documented on help page as `archive-crc`; 2013 script used this exact form) | Inner RAR file CRC32 from the last volume's RAR header (needs 1 article, the last volume, or the RAR header of any volume that carries the file CRC) | Release dirnames matching the archived CRC, plus `resultsCount` (per 2013 script). Details of the archived filename come from a follow-up `details` call | alt / twin (if the CRC+size match, the scene release is the same bytes) | 1 article + 1 API call | https://www.srrdb.com/help ; https://pastebin.com/3JEHRfU6 |
| Inner-file size (secondary, to confirm CRC hit) | `archive-size:<bytes>` keyword (documented on help page). Combined form `archive-crc:X archive-size:Y` (space/AND syntax) UNVERIFIED | yEnc-declared size of the inner file, or RAR header unpacked size | Same result shape as CRC search, narrowed | alt / twin when combined with CRC; weak alone | free (yEnc header) + 1 API call | https://www.srrdb.com/help |
| Stored-sample CRC (sample is a scene sample) | `store-real-crc:<CRC32>` keyword (help page: CRC32 of the complete stored sample inside the SRS) | Sample file CRC from a posted sample (needs the sample article; sample CRC is not in yEnc header) | Release dirnames | alt / twin of the sample | a few articles + 1 API call | https://www.srrdb.com/help |
| Stored-filename | `store-real-filename:<name>` keyword | Name is obfuscated, so this only helps if the name is already known (e.g. from an NFO) | Release dirnames | same film (weak; name-based) | 1 article (NFO) + 1 API call | https://www.srrdb.com/help |
| Release-level details (after a hit) | `GET https://www.srrdb.com/api/details/<dirname>` | A known dirname (from a CRC hit) | `files[]` (fields `name`, `size`, `crc`) for confirmed releases; `adds[]` (name, size, crc, id of .srs) for unconfirmed releases, which hides the video's size/CRC (issue 116). Field `archived-files` exists per Go client struct (`json:"archived-files"`); `Files` also present. Exact shape UNVERIFIED | Confirms which inner file matched (tool), and gives full file list for size/CRC cross-check | 1 API call per release | https://www.srrdb.com/api/details/Harry.Potter.And.The.Chamber.Of.Secrets.2002.DVDRip.XViD-iNTERNAL-TDF (quoted in https://bitbucket.org/srrdb/srrdb-issues/issues/91/files-element-in-search-api); https://pkg.go.dev/github.com/hashworks/go-srrdb-API/srrdb |
| Release name exact (after name is known) | `r:<dirname>` keyword ("faster than normal search, used best in api") | Dirname | Release record | same film / same release | 1 API call | https://www.srrdb.com/help |
| IMDb link | `imdb:tt<id>` keyword; Go client `IMDB()` | IMDb id from an NFO (needs 1 NFO article) | `releases` list (imdb, title, rating, votes per Go struct) | same film | 1 article + 1 API call | https://pkg.go.dev/github.com/hashworks/go-srrdb-API/srrdb |
| NFO fetch | Go client `NFO(releasename)` returns `nfo`, `nfolink`. Path UNVERIFIED | Dirname (needs a hit first) | NFO text | same film / same uploader (group tag in NFO) | 1 API call | https://pkg.go.dev/github.com/hashworks/go-srrdb-API/srrdb |
| SRR download by dirname | Go client `Download(dirname)` returns SRR bytes. Path UNVERIFIED | Dirname | Full SRR (RAR block headers, stored SFV/NFO/SRS) | alt / twin: local parse of SRR gives exact inner names and sizes for every volume | 1 API call + local parse | https://pkg.go.dev/github.com/hashworks/go-srrdb-API/srrdb |
| SFV / volume CRC | No SFV search keyword documented. Volume CRCs in SFV are NOT the inner-file CRC (UNVERIFIED that `archive-crc` will not match them) | SFV article | Nothing direct | Would only work if srrDB indexes volume CRCs (UNVERIFIED) | 1 article | https://www.srrdb.com/help |
| xREL release search | `GET /search/releases.json?q=<keyword>&scene=true&limit=25` (api.xrel.to; OpenAPI spec) | Name or group keyword only | Release objects: `dirname`, `size` (number + unit, release-level), `group_name`, `time` | same film by name only; no per-file CRC/size | free, rate limited ("additional search rate limiting applies", no numbers) | https://raw.githubusercontent.com/xREL-to/OpenAPI/master/xRELOpenAPI.yaml |
| predb (predb.net) | No API documentation found. Site shows files and size per release (UNVERIFIED that per-file CRC/size is exposed) | Name | Unknown | Unknown | Unknown | https://predb.net/api (no docs returned) |
| SRR block parse (local, no API) | Local parse of the SRR file: 0x69 volume header (app name), 0x6A stored file (ADD_SIZE = stored file size), 0x71 RAR subblock (names the .rar), 0x72 RAR marker, 0x73 RAR archive header (HEAD_FLAGS; 0x0080 MHD_PASSWORD = headers encrypted) | An SRR file from srrDB download | Per-volume RAR headers, stored file sizes, names | Exact inner names/sizes; proves alt/twin when CRC+size match | 1 SRR download + local CPU | https://bitbucket.org/Gfy/pyrescene/issues/190/password-protected-rar-set-creates-huge (quotes spec layout; raw srr_spec.txt not reachable) ; https://github.com/srrDB/pyrescene |

## Answers to the key questions

- Search by inner-file CRC32: YES, via `api/search/archive-crc:<hex>` (help keyword `archive-crc`; the 2013 script confirms the form). UNVERIFIED that current API still answers in this form, because the fetcher could not reach `/api/`.
- Search by size: a keyword `archive-size` is documented. Its exact meaning (inner file vs archive) and combined syntax are UNVERIFIED.
- Returned fields: `resultsCount` and `release` are confirmed by the 2013 script. Details: `files`/`archived-files` with `name`/`size`/`crc` per Go client; `adds` for unconfirmed. `sfv` field not found in any source.

## Rate limits, terms, dumps, keys

- Rate limit: none stated on help page or in any client; UNVERIFIED.
- robots.txt: `/api/` fetches were refused as robots_disallowed. Automated bulk use may breach site policy. Treat as a risk, not a permitted path, until srrDB operators are asked.
- API key: none mentioned (Go client uses a login cookie jar only for uploads).
- Dumps: help page says "release lists and data dumps are automatically made available" (no URL). Issue 124 reports the `/open` link redirects to home; closed as "works as intended"; login may be needed. UNVERIFIED.
- Clients: Go `hashworks/go-srrdb-API`; Go `hashworks/srrdb-Terminal-Client` (Arch package 1.3.0-1). No Python client found.

## Other notes

- Encrypted RAR sets (MHD_PASSWORD in the RAR archive header) hide the inner names and CRCs from the SRR, so this oracle does not work for them without the password.
- pyrescene reads SRR as Python 2.6/2.7/3 code. The SRS sample format is for video samples only (AVI/MKV/MP4/WMV), not RAR.
- Rust implementations of SRR parsing were not found in this run.

## Takeaways

- The useful oracle is `archive-crc:<inner CRC32>` plus `archive-size:<bytes>`: one article, then one search call, gives candidate scene dirnames. It works only for non-encrypted RAR sets.
- Free yEnc sizes are a cheap pre-filter; use them to skip a search when the size is absent from a candidate release's `files`.
- Confirmed releases expose full `files` (name/size/CRC) in details; unconfirmed releases hide the video's size and CRC, so do not rely on details for those.
- srrDB `/api/` is robots-disallowed and has no published rate limit or terms for bulk use. Ask srrDB before automating at volume.
- xREL and predb give name-level or release-level data only; neither exposes inner-file CRCs in documented sources.

Sources: https://www.srrdb.com/help ; https://pastebin.com/3JEHRfU6 ; https://bitbucket.org/srrdb/srrdb-issues/issues/91/files-element-in-search-api ; https://bitbucket.org/srrdb/srrdb-issues/issues/116/api-provides-details-of-srs-file-instead ; https://bitbucket.org/srrdb/srrdb-issues/issues/124/open-data-dumps-not-available ; https://pkg.go.dev/github.com/hashworks/go-srrdb-API/srrdb ; https://github.com/hashworks/srrdb-Terminal-Client ; https://github.com/srrDB/pyrescene ; https://bitbucket.org/Gfy/pyrescene/issues/190/password-protected-rar-set-creates-huge ; https://raw.githubusercontent.com/xREL-to/OpenAPI/master/xRELOpenAPI.yaml
