# w4b: Metadata tag vocabulary in MKV and MP4 (research pass)

Marks: [V] = verified from fetched source this session. [M] = memory/unverified. Hard limit 10 min; several sources failed (see Sources).

Sources:
- IETF cellar tags draft-16 (April 2025): https://www.ietf.org/archive/id/draft-ietf-cellar-tags-16.html [V]
- Matroska tagging mirror (robux4 github pages): https://robux4.github.io/matroska-specification/tagging.html [V, partial]
- libmp4v2 iTunesMetadata wiki: https://www.github.com/sergiomb2/libmp4v2/wiki/iTunesMetadata [V]
- Failed: raw.githubusercontent.com tagging.md (client_error); matroska.org (not attempted after raw failure, memory says 403)
- Not fetched, search hits only: https://wiki.multimedia.cx/index.php/FFmpeg_Metadata ; https://developer.apple.com/documentation/quicktime-file-format/quicktime_metadata_keys.md ; https://exiftool.org/forum/index.php?msg=92108 (handler/vendor/brand identification) ; https://community.adobe.com/questions-729/xmp-and-mp4-exported-separately-1347700

---

## A. Matroska official tag names (IETF draft-16 section 4 unless marked)

TargetTypeValue (summary table from robux4 mirror [V, partial]):
- 70 COLLECTION
- 60 EDITION / ISSUE / VOLUME / OPUS (also SEASON / SEQUEL in video table per draft)
- 50 ALBUM / OPERA / CONCERT (video: MOVIE / EPISODE)
- 40 PART / SESSION
- 30 TRACK / SONG (video: CHAPTER)
- 20 SUBTRACK / MOVEMENT (video: SCENE)
- 10 SHOT (video table) [V, but the audio/video split is inconsistent across sources; treat exact names as M for levels 60/50/30/20]

```
tag: TITLE / SUBTITLE
container: matroska
meaning: title string; subtitle (level 30 [V])
example: "Blade Runner 2049"
who_writes: taggers (tinyMediaManager, MKVToolNix GUI), some rippers
linking_value: alt (title text, not unique)
privacy: no
```
```
tag: IMDB / TMDB / TVDB / TVDB2
container: matroska
meaning: external IDs. IMDB "tt"+7 digits. TMDB prefix "movie/" or "tv/". TVDB series/episode id. TVDB2 prefix "series/", "episodes/", "movies/" [V]
example: IMDB=tt1856101 ; TMDB=movie/335984
who_writes: tinyMediaManager, Sonarr/Radarr-adjacent tools, manual [M for who]
linking_value: same-film (strongest title key)
privacy: no
```
```
tag: ISRC
container: matroska
meaning: recording code, hyphens kept, no prefix [V]
example: USSM10012345
who_writes: music taggers, label masters
linking_value: same-film for audio tracks / same-source
privacy: no
```
```
tag: ENCODER / ENCODER_SETTINGS
container: matroska
meaning: software or hardware that encoded the item; encoder settings string (no format) [V]
example: ENCODER="x264 core 164 r3095" ; ENCODER_SETTINGS="cabac=1 / ref=3 ..."
who_writes: encoder (x264/x265/HandBrake), ffmpeg/mkvmerge writing paths [M for which tool writes which]
linking_value: twin (same encoder build and settings = same encode chain)
privacy: no
```
```
tag: ENCODED_BY
container: matroska
meaning: person or entity who encoded (ID3 TENC analogue) [V]
example: a username or group name
who_writes: uploader/encoder workflow
linking_value: uploader
privacy: yes (can be a real name or handle)
```
```
tag: DATE_RELEASED / DATE_RECORDED / DATE_ENCODED / DATE_TAGGED
container: matroska
meaning: UTC dates. DATE_ENCODED and DATE_TAGGED are file-side; DATE_RELEASED/RECORDED are production-side [V]
example: 2017-10-06 00:00:00
who_writes: taggers, encoders (DATE_ENCODED) [M]
linking_value: same-source (DATE_ENCODED to the second is a fingerprint of one mux run)
privacy: no
```
```
tag: ORIGINAL_MEDIA_TYPE / CONTENT_TYPE / ORIGINAL (nested) / SAMPLE (nested)
container: matroska
meaning: media the material came from (e.g. disc/format). ORIGINAL and SAMPLE describe the source work [V on existence, M on value examples]
example: ORIGINAL_MEDIA_TYPE=BD ; CONTENT_TYPE=Movie
who_writes: taggers, rippers [M]
linking_value: same-source (source format hint)
privacy: no
```
```
tag: DISTRIBUTED_BY / PRODUCTION_STUDIO / LABEL / PUBLISHER / PURCHASE_OWNER / PURCHASE_INFO / PURCHASE_ITEM / PURCHASE_PRICE / PURCHASE_CURRENCY
container: matroska
meaning: store / distributor / purchase metadata [V names; value semantics M]
example: DISTRIBUTED_BY=Studio; PURCHASE_OWNER=<name>
who_writes: taggers, purchase-aware tools [M]
linking_value: store (source) ; PURCHASE_OWNER is uploader-type
privacy: PURCHASE_OWNER yes, PURCHASE_INFO maybe (store account), others no
```
```
tag: COMMENT / PLAY_COUNTER / RATING
container: matroska
meaning: user-personal tags [V: draft says these are personal to the user]
example: COMMENT="ripped by me"
who_writes: the user, players
linking_value: uploader (free-text comments may name the uploader)
privacy: yes (user-personal)
```
```
tag: SUMMARY / SYNOPSIS / DESCRIPTION / KEYWORDS / SUBJECT / LAW_RATING / PERIOD / GENRE / MOOD
container: matroska
meaning: text description and classification; LAW_RATING = content rating [V]
example: LAW_RATING=R
who_writes: taggers
linking_value: none (descriptive)
privacy: no
```
```
tag: DURATION / NUMBER_OF_FRAMES / NUMBER_OF_BYTES / BPS / FPS
container: matroska
meaning: per-track stats. DURATION, NUMBER_OF_FRAMES, NUMBER_OF_BYTES are per-track in mkvmerge-style output; BPS and FPS are technical [V names in draft; "written by mkvmerge" M]
example: NUMBER_OF_BYTES=1234567890
who_writes: mkvmerge (statistics tags) [M], ffmpeg with -write_stats/-metadata per track [M]
linking_value: twin (exact byte/frame counts fingerprint one mux)
privacy: no
```
```
tag: _STATISTICS_WRITING_APP / _STATISTICS_WRITING_DATE_UTC / _STATISTICS_TAGS (underscore names)
container: matroska
meaning: mkvmerge-style statistics tags naming the writing app and mux time [M, not verified in fetched text]
example: _STATISTICS_WRITING_APP=mkvmerge v... ; _STATISTICS_WRITING_DATE_UTC=...
who_writes: mkvmerge [M]
linking_value: same-source (writing app + date) / twin (exact date string)
privacy: no
```
```
tag: REPLAYGAIN_GAIN / REPLAYGAIN_PEAK / EBU_R128_LOUDNESS / EBU_R128_MAX_TRUE_PEAK / EBU_R128_LOUDNESS_RANGE (+ momentary, short-loudness)
container: matroska
meaning: loudness measurement of the audio. EBU ones are binary [V]
example: REPLAYGAIN_GAIN=-6.2 dB
who_writes: audio normalizers (ffmpeg loudnorm, mkvmerge-aware tools) [M]
linking_value: same-source weak (loudness is a re-encode fingerprint)
privacy: no
```
```
tag: TagTrackUID / TagEditionUID (Targets, not names)
container: matroska
meaning: binds a Tag to a specific track or edition [M]
linking_value: none alone
privacy: no
```
```
tag: MCDI / ISBN / BARCODE / CATALOG_NUMBER / LABEL_CODE / LCCN
container: matroska
meaning: external release identifiers. MCDI binary (CD TOC) [V names]
example: BARCODE=0123456789012
linking_value: same-source (identifies a specific physical release)
privacy: no
```
```
tag: LEAD_PERFORMER / ARTIST / DIRECTOR / ACTOR / CHARACTER / WRITTEN_BY / PRODUCER / ... (entity tags)
container: matroska
meaning: credits. Roughly 30 role names [V]
linking_value: none (cast/crew are shared across many films, not unique)
privacy: no (public credits), but identifies the tagger if the tagger filled in a private note
```
```
tag: TOTAL_PARTS / PART_NUMBER / PART_OFFSET
container: matroska
meaning: multipart position [V]
linking_value: same-source (multi-disc / multi-part groupings)
privacy: no
```
Adjacent, NOT Tags (EBML Segment/Track elements) [M, memory]:
- Segment MuxingApp / WritingApp: e.g. "libmkv" / "mkvmerge v..." / "HandBrake". Strong tool fingerprint.
- TrackEntry Name: track labels such as "DTS-HD MA 5.1", "English [Forced]". Release-group habit.
- Scene rule on title: scene MKVs are expected to keep the title/tag blank or equal to the release name [M, not verified against scene rules this session].

---

## B. MP4 iTunes atoms (libmp4v2 wiki [V] unless noted)

```
tag: ©nam / ©ART / aART / ©alb / ©wrt / ©gen / gnre / ©cmt / desc / ldes / sonm / soar / cprt / catg / keyw / pcst / purl
container: mp4 (ilst)
meaning: standard iTunes text fields (name, artist, album artist, album, composer, genre, comment, descriptions, sort names, copyright, podcast) [V]
example: ©ART=<actor/artist>
who_writes: Apple/iTunes purchases; taggers (Subler, AtomicParsley, mutagen)
linking_value: alt (text, not unique)
privacy: no
note: gnre vs ©gen: only one allowed at a time [V]
```
```
tag: ©day
container: mp4
meaning: release date, YYYY-MM-DD, can be partial [V]
linking_value: alt
privacy: no
```
```
tag: ©too
container: mp4
meaning: encoding tool (writer) [V name; the ffmpeg "Lavf..." and HandBrake values are M]
example: ©too=HandBrake 1.x
linking_value: twin / same-source (tool fingerprint)
privacy: no
```
```
tag: tvsh / tven / tvsn / tves / tvnn
container: mp4
meaning: TV show name, episode ID, season (int32), episode (int32), network [V]
linking_value: alt (series/episode key with tvsh+tvsn+tves)
privacy: no
```
```
tag: stik
container: mp4
meaning: media type, 8-bit enum: 0 = Movie (old), 1 = Normal (music), 2 = Audiobook, 6 = Music Video, 9 = Movie, 10 = TV Show, 11 = Booklet, 14 = Ringtone [V]
linking_value: none
privacy: no
```
```
tag: hdvd
container: mp4
meaning: HD video flag, 8-bit boolean [V]. The 0/1/2/3 mapping (SD/720p/1080p/4K) is M, not on the fetched page.
linking_value: none
privacy: no
```
```
tag: rtng
container: mp4
meaning: content rating enum: 0 = none, 2 = clean, 4 = explicit [V]
linking_value: none
privacy: no
```
```
tag: covr
container: mp4
meaning: cover art, JPEG/PNG/BMP, may have multiple data atoms [V]
linking_value: twin possible (identical cover bytes across releases), weak
privacy: no
```
```
tag: cnID
container: mp4
meaning: iTunes catalog ID, used to combine SD and HD encodes of the same title [V]
example: 32-bit integer
who_writes: iTunes/Apple TV purchase rips; taggers
linking_value: SAME-FILM (strongest for iTunes-sourced files)
privacy: no
```
```
tag: sfID
container: mp4
meaning: iTunes Store country of purchase, country-specific int (e.g. 143441 = USA, 143444 = GBR) [V]
linking_value: same-source (storefront)
privacy: no (country only, but narrows the buyer)
```
```
tag: akID
container: mp4
meaning: store account type, 0 = iTunes, 1 = AOL [V]
linking_value: none
privacy: low
```
```
tag: apID
container: mp4
meaning: purchase account (Apple ID, usually an email) [V]
linking_value: uploader / buyer identity
privacy: YES. Flag as "store but never display". This is the buyer.
```
```
tag: purd
container: mp4
meaning: purchase date, UTF-8 string [V]
linking_value: same-source (purchase time)
privacy: YES (identifies a buy event with apID)
```
```
tag: atID / plID / geID
container: mp4
meaning: unclear, marked "Use?" on the wiki [V]
linking_value: unknown
privacy: unknown, treat as yes until checked
```
```
tag: egid / cmID / ownr / xid
container: mp4
meaning: NOT on the fetched page [M]. Memory: egid = episode global ID (podcast/TV); xid = vendor ID string; ownr = owner name; cmID = content match ID.
linking_value: xid and egid M same-film/series candidates; ownr = uploader/buyer
privacy: ownr yes (M)
```
```
tag: ----:com.apple.iTunes:iTunEXTC / ----:com.apple.iTunes:iTunMOVI
container: mp4 (freeform)
meaning: rating string "mpaa|R|400|" and a plist with cast/directors/studio [M, not on fetched page]
linking_value: same-film (cast/studio list) / none for rating
privacy: no
```

Remux behavior: whether these survive remux (ffmpeg default copies global metadata; mkvmerge does not carry MP4 ilst) is M, not verified.

---

## C. QuickTime mdta and XMP (mostly memory; not verified)

```
tag: com.apple.quicktime.* (mdta keys, e.g. location, make/model, software, creationdate, author) [M]
container: mp4 (mdta in moov/meta)
linking_value: same-source (software string = tool), location = privacy yes
privacy: location yes; software no
```
```
tag: XMP in uuid box be7acfcb-97a9-42e8-9c71-999491e3afac [M, UUID value from memory]
container: mp4
meaning: XMP packet. Adobe Premiere writes it with xmpMM:DocumentID and xmpMM:InstanceID, plus xmp:CreatorTool [M]
example: xmpMM:DocumentID=xmp.did:<uuid>
who_writes: Adobe Premiere / Media Encoder [M]
linking_value: twin (DocumentID is per-export unique; matching DocumentID across files = same export lineage) ; CreatorTool = tool
privacy: low (CreatorTool no; DocumentID is an export ID, not a person)
```
```
tag: major_brand / compatible_brands / VENDOR_ID / HANDLER_NAME
container: mp4
meaning: ftyp brand (isom, mp42, M4V), track handler vendor (ffmpeg usually writes "[0][0][0][0]"; Apple writes "appl"), handler names like "VideoHandler" / "SoundHandler" [M]
linking_value: same-source (muxer fingerprint), none for brand alone
privacy: no
```

---

## D. Summary of linking value and privacy

- Same-film keys: cnID (iTunes), IMDB, TMDB, TVDB, TVDB2, ISRC (audio), tvsh+tvsn+tves (series), xid/egid (M).
- Same-source / twin keys: ENCODER + ENCODER_SETTINGS, DATE_ENCODED to the second, NUMBER_OF_BYTES, MuxingApp/WritingApp (adjacent), sfID, Premiere DocumentID (M), ©too.
- Privacy yes: apID, purd, ownr (M), ENCODED_BY, PURCHASE_OWNER, COMMENT, PLAY_COUNTER, RATING, QuickTime location (M), uploader names in free-text COMMENT / SUMMARY.
- Never display: apID, purd, ownr, ENCODED_BY, PURCHASE_OWNER.

## E. Not done (time limit)

- matroska.org and raw github tagging.md failed. Used the IETF draft and a mirror instead.
- Scene rules (x264 TV/HD, BluRay) not checked.
- FFmpeg metadata wiki, Apple quicktime_metadata_keys, exiftool forum threads: found by search, not fetched.
- MakeMKV tag specifics and tinyMediaManager/Subler/HandBrake tag writes: memory only.
