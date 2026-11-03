# W4b: archive file order, RAR volume headers, RAR5 Quick Open (source-verified)

Scope: unrar source (pmachapman/unrar mirror), 7-Zip source (ip7z/7zip), Go archive/zip writer. The RAR archiver (rar), rar.txt and the RAR docs are not in any fetched source. Every claim below is tagged VERIFIED (source file + function/line) or UNVERIFIED.

Local copies: /tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w4b/ (unrar files), and the src_*.cpp files in the same folder (7-Zip, Go).

## Source status

| Item | Result |
|---|---|
| unrar qopen.cpp / qopen.hpp | fetched, read in full |
| unrar volume.cpp | fetched, read in full |
| unrar arcread.cpp | fetched, read the relevant parts |
| unrar cmddata.cpp, options.hpp, options.cpp | fetched, read the relevant parts |
| unrar archive.cpp | fetched, read Read/Seek/Tell |
| unrar headers.hpp (RAR4), headers5.hpp (RAR5) | fetched, flag defines read |
| 7-Zip UI/Common/SortUtils.cpp, Archive/7z/7zUpdate.cpp, 7zUpdate.h, 7zHandlerOut.cpp, 7zOut.cpp, UI/Common/UpdatePair.cpp | fetched, read the relevant parts |
| 7-Zip Archive/Zip/ZipUpdate.cpp | fetched, grepped for sort: no match |
| Go src/archive/zip/writer.go | fetched, read CreateHeader and Close |
| rar.txt | NOT AVAILABLE. rarlab.com blocked. raw.githubusercontent.com/rarlab/rar/master/rar.txt returned 404. Other guessed paths returned 404 or were not fetchable. |

## 1. RAR: file add order (archiver side)

The archiver is not in unrar. unrar only parses the switches below, so the add-order behaviour itself is UNVERIFIED.

- VERIFIED: `-ds` sets `DisableSortSolid=true`. unrar cmddata.cpp, `CommandData::GetSwitchValue`-style switch parser, `case 'D'` / `case 'S'` (around line 406). Field declared at options.hpp:179 (`bool DisableSortSolid;`). Name suggests "do not sort files when adding to a solid archive", but the help text is not in the fetched source, and the sort itself happens in the archiver.
- VERIFIED: `-ep`, `-ep1`, `-ep2`, `-ep3`, `-ep4` map to `PATH_EXCL_MODE` values (cmddata.cpp about lines 430-445; options.hpp:10-15). Enum comments: `EXCL_SKIPWHOLEPATH` = `-ep` (exclude the path completely), `EXCL_BASEPATH` = `-ep1` (exclude the base part of the path), `EXCL_SAVEFULLPATH` = `-ep2`, `EXCL_ABSPATH` = `-ep3`. In unrar these affect extraction paths. Their effect on names stored by the archiver is UNVERIFIED.
- VERIFIED: `-r` (RECURSE_ALWAYS), `-r-` (RECURSE_DISABLE), `-r0` (RECURSE_WILDCARDS), `-ri` (cmddata.cpp about lines 843-856; options.hpp:48-52). The traversal order of recursion is UNVERIFIED (archiver side).
- UNVERIFIED: default sort for solid archives (by extension, then name?). unrar has no code for it.
- UNVERIFIED: order for non-solid archives (filesystem enumeration order vs sorted). unrar has no code for it.
- UNVERIFIED: how path separators are stored (RAR5 uses `/` in stored names). Not visible in the parsing code fetched. Header flag `LHD_UNICODE` / `FHFL_*` only.

## 2. RAR volumes: which headers appear in which volume

- VERIFIED: `MergeArchive()` (volume.cpp:10-193). It closes the current volume (line 36), computes the next name (`NextVolumeName`, lines 38-39), and opens it (lines 74-127). After the open, `Arc.CheckArc()` runs (line 142). The next header is read as follows:
  - `SplitHeader = (HEAD_FILE or HEAD_SERVICE) && hd->SplitAfter` (lines 15-17).
  - If `SplitHeader`, it calls `Arc.SearchBlock(HeaderType)` (line 158-159). Otherwise it calls `Arc.ReadHeader()` (line 161).
  - `DataIO->UnpVolume = hd->SplitAfter` (line 179). `DataIO->SetPackedSizeToRead(hd->PackSize)` (line 180).
- VERIFIED: RAR5 flags `HFL_SPLITBEFORE = 0x0008`, `HFL_SPLITAFTER = 0x0010` (headers5.hpp:16, 18). Read in `ReadHeader50` at arcread.cpp:867-868.
- VERIFIED: RAR4 flags `LHD_SPLIT_BEFORE = 0x0001`, `LHD_SPLIT_AFTER = 0x0002` (headers.hpp:39-40). Read in `ReadHeader15` at arcread.cpp:257-258.
- VERIFIED (reader requirement): when a file's data continues into volume N+1, unrar expects the first block of N+1 to be a continuation header for that file (`SearchBlock` for the same header type, volume.cpp:158-159). So the volume that holds a file's data start has the header with SplitAfter set, and the next volume holds a continuation header with SplitBefore set. A volume that holds data of a file therefore holds a header for it.
- UNVERIFIED (archiver side): whether a volume holds headers for files that have no data in it (empty files, directories, files fully in other volumes). unrar only tells us about the continuation case.
- Practical reading for an indexer: a file that starts in vol 1 and runs into vol 2 shows up as a header with SplitAfter in vol 1 and a header with SplitBefore in vol 2. Its data "lands" in both volumes. The reader code above is what makes this hold.

## 3. RAR5 Quick Open (QO)

Where it lives and how it is found:
- VERIFIED: the QO block is a HEAD_SERVICE block with sub-type name `QO` (`SUBHEAD_TYPE_QOPEN`, headers.hpp:119). `QuickOpen::Load` checks `Arc->SubHead.CmpName(SUBHEAD_TYPE_QOPEN)` (qopen.cpp, `Load`).
- VERIFIED: it is pointed to by the main header locator extra field (`MHEXTRA_LOCATOR`, flag `MHEXTRA_LOCATOR_QLIST = 0x01`, headers5.hpp:68, 72). `ProcessExtra50` stores `hd->QOpenOffset = Offset + CurBlockPos` (arcread.cpp:1012-1020). The offset is relative to the main header block of the same file.
- VERIFIED (comment in source): arcread.cpp:792 says "We seek to QO block in the end of archive when processing QOpen.Load". So the block is normally at the end of the file. The archiver placement rule itself is UNVERIFIED.
- VERIFIED: loading happens inside `ReadHeader50`, case HEAD_MAIN (arcread.cpp:788-805). Conditions: `!ProhibitQOpen && MainHead.Locator && MainHead.QOpenOffset>0 && Cmd->QOpenMode!=QOPEN_NONE`. It calls `QOpen.Init(this,false)` then `QOpen.Load(MainHead.QOpenOffset)`. `ProhibitQOpen` guards against recursion (qopen.cpp, `Load` comment).

Per volume or whole archive?
- VERIFIED: QO is per file. Each volume's main header is read by its own `ReadHeader50` call, so each volume's own locator is used. The `QuickOpen` object is bound to the open `Archive` (`QOpen.Init(this,false)`).
- VERIFIED: every cached header position is computed as `LastReadHeaderPos = QOHeaderPos - Offset` (qopen.cpp, `ReadNext`). The offset is relative to the QO block, and reads go through `Arc->File::Read` on the same file (`ReadBuffer`). So a QO block can only describe headers that sit in its own volume file.
- Conclusion (VERIFIED from the offset arithmetic): QO in volume N caches only headers stored in volume N. It does not cover other volumes. Whether it covers all headers in that volume or only some is UNVERIFIED, since the writer is not in unrar.
- UNVERIFIED: whether the archiver writes QO into every volume or only the last one. Only the reader side is in unrar. unrar handles a QO block in any volume it opens, so both are compatible with the reader.

What QO contains and how it is read:
- VERIFIED: a QO block is a stream of records: CRC50, then the size fields, then Flags, Offset, HeaderSize, and the raw header bytes (`QuickOpen::ReadRaw`, `QuickOpen::ReadNext`). Each record is a copy of a header, not file data. It is checked with `SavedCRC==Raw.GetCRC50()`.
- VERIFIED: `QuickOpen::Load` reads the QO block in 64 KiB chunks (`MaxBufSize=0x10000`, qopen.hpp; `ReadBuffer`). If the QO block is encrypted and no password is set, `Loaded=false` (qopen.cpp `Load`), and QO is silently skipped.
- VERIFIED: the public API is `Read`, `Seek`, `Tell`. `Archive::Read` calls `QOpen.Read` first and falls back to `File::Read` (archive.cpp, `Archive::Read`, about lines 320-325). `Archive::Seek` calls `QOpen.Seek` first (archive.cpp, `Archive::Seek`, about lines 328-333). `Archive::Tell` calls `QOpen.Tell` (archive.cpp, about lines 336-340).
- VERIFIED: `QuickOpen::Read` advances `ReadNext` until a cached header covers `SeekPos`, then copies from the cache (qopen.cpp `Read`). Any mismatch sets `Loaded=false` and the normal file reads take over.
- Net effect (VERIFIED): QO is a transparent header-read cache for sequential header scanning. It is not a separate file list. An indexer that only needs names and positions still has to walk headers.

Defaults and switches:
- VERIFIED: default `QOpenMode = QOPEN_AUTO` (options.cpp:21, inside `RAROptions::Init`). `CommandData::Init` calls `RAROptions::Init` (cmddata.cpp:12-18).
- VERIFIED: switches (cmddata.cpp about lines 823-838, enum options.hpp:72):
  - `-qo` = QOPEN_AUTO
  - `-qo-` = QOPEN_NONE (QO is not loaded)
  - `-qo+` = QOPEN_ALWAYS
- VERIFIED: in the reader, AUTO and ALWAYS behave identically. The only check is `Cmd->QOpenMode != QOPEN_NONE` (arcread.cpp:789). The difference, if any, must be on the writer side, which is UNVERIFIED.
- VERIFIED: `-qo` is flagged as an incompatible switch when `QOpenMode != QOPEN_AUTO` (cmddata.cpp about lines 1287-1288, UIERROR_INCOMPATSWITCH). The exact format condition around that line is UNVERIFIED (not fully read).
- UNVERIFIED: whether `rar` (the archiver) writes QO by default. Only the reader defaults are in unrar.

## 4. 7-Zip: file order inside solid archives

Sort code (7-Zip, ip7z/7zip main, CPP/7zip/Archive/7z/7zUpdate.cpp):
- VERIFIED: `CompareUpdateItems` (lines 850-918).
  - Directories are handled first (lines 864-872), and sorted by name.
  - For non-directory files, if `sortByType` is false, the comparison is `CompareFileNames(u1.Name, u2.Name)` on the full path (line 918 area: `RINOK(CompareFileNames(u1.Name, u2.Name))`), then `IndexInClient`, then `IndexInArchive`. So the default order is full path name, case-insensitive per `CompareFileNames`.
  - If `sortByType` is true (lines 877-886), the comparison is: extension index (`ExtensionIndex`), then extension string, then base name, then mtime (with undefined mtimes last), then size.
- VERIFIED: `ExtensionIndex` comes from `GetExtIndex` (line 755 onward), which looks up a built-in list `g_Exts` (archive types first: `7z xz lzma ace arc arj bz tbz bz2 tbz2 cab deb gz ...`, line 769-771 region). Known extensions sort in list order, unknown extensions sort after the list.
- VERIFIED: where the sort is applied (7zUpdate.cpp lines 2706-2717): `sortByType = options.UseTypeSorting`, then `refItems.Sort(CompareUpdateItems, ...)`.
- VERIFIED: the default is OFF for the 7z handler, even though the `COptions` constructor says `UseTypeSorting(true)` (7zUpdate.h:142). `COutHandler::InitProps7z` sets `_useTypeSorting = false` (7zHandlerOut.cpp:872). `InitProps` calls `InitProps7z` (7zHandlerOut.cpp:879-883). `SetOptions` copies `_useTypeSorting` into `options.UseTypeSorting` (7zHandlerOut.cpp:800). So the effective default is no type sorting. The property name `qs` (`-mqs`) sets it (7zHandlerOut.cpp:1044).
- Caveat: this conflicts with the common claim that 7-Zip sorts by extension by default. The source fetched says no for the 7z handler. The GUI or CLI may pass `qs=on` separately. UNVERIFIED for the GUI/CLI path (ArchiveCommandLine.cpp has no `mqs` or `qs` match, so the CLI itself does not set it).

Solid block boundaries:
- VERIFIED: sorted items are grouped by solid group (`CSolidGroup`, 7zUpdate.cpp about line 2000-2100, `isSolid = numSolidFiles > 1 && NumSolidBytes != 0`, line 2061). Each block takes up to `numSolidFiles` consecutive items in sort order (`for (numSubFiles = 0; i + numSubFiles < numFiles && numSubFiles < numSolidFiles; numSubFiles++)`, line 2745). So the first solid folder (the first block in the archive) holds the first N items of the sorted list, where N is the file-count limit. The byte limit (`NumSolidBytes`) also closes blocks, but the exact code was not read (UNVERIFIED for byte-limit details).
- Consequence for an indexer: with default settings, file 1 of the first block is the first item by full path, not by extension. Set `-mqs` or `qs=on` to get extension order.

Directories and empty files:
- VERIFIED: directories are appended to `newDatabase.Files` first (7zUpdate.cpp:2250-2253). Non-AUX directories and empty files are written next (comment "Write non-AUX dirs and Empty files", line 2262; `emptyRefs` sorted with `CompareEmptyItems`, line 2284). Data-bearing files come last, in `refItems` sorted order (`newDatabase.Files.Add(file)`, line 2734). So the file-record order in the header is: directories, then empty files, then solid-block files in sort order.
- VERIFIED: `CompareEmptyItems` (line 703-718) sorts non-anti before anti, directories before files, then by name.
- UNVERIFIED: that `emptyRefs` items are appended to `newDatabase.Files` in that loop (the loop was not read to its end). The position of directories and empty files relative to data files in the final header follows from the Add order above.

Multi-volume (.7z.001):
- VERIFIED: `COutArchive::WriteDatabase` (7zOut.cpp:883-930) writes the header at the current position of the output stream, after the packed streams. The start header at offset 0 is a placeholder that gets patched later (`WriteStartHeader`, 7zOut.cpp:68; signature write at about lines 108-130). `NextHeaderOffset` in the start header points to the header at the end of the logical stream.
- Consequence (VERIFIED for logical layout): the file list (the header) is at the end of the logical 7z stream, so it lands in the last volume. The start header (32 bytes) is at the start of volume 1.
- UNVERIFIED: the volume splitter itself (the stream layer that cuts the logical stream into `.7z.001`, `.7z.002`, ...). Not fetched.

## 5. ZIP: entry order

- VERIFIED (Go archive/zip writer, not 7-Zip or Info-ZIP): `Writer.CreateHeader` appends each entry to `w.dir` and writes the local file header at once (writer.go, `CreateHeader`, about lines 398-399). So entries are written in add order. `Writer.Close` writes the central directory after all entries (writer.go:82-97, `Close`). So the central directory is at the end.
- VERIFIED: 7-Zip ZipUpdate.cpp contains no `Sort` or `sort` identifiers (grep, no match). So 7-Zip's ZIP update does not re-sort by type or name. Its add order is the update-item order, which was not traced further (UNVERIFIED for exact order).

## 6. Open items (UNVERIFIED)

- RAR archiver add order: solid default sort, non-solid enumeration order, `-ds` effect, `-ep1` effect on stored names, `-r` traversal order, path separators in stored names. Needs rar.txt or the rar archiver source.
- RAR archiver placement of QO (before or after ENDARC, one per volume or last volume only), and whether QO covers all headers of that volume.
- RAR writer rule for headers in volumes that hold no data of the file.
- 7-Zip `-mqs` default in the GUI and CLI; exact byte-limit block splitting; the volume splitter.
- 7-Zip ZIP entry order (only the Go writer was verified).

## Sources

- https://raw.githubusercontent.com/pmachapman/unrar/master/qopen.cpp (QuickOpen::Load, Read, Seek, ReadBuffer, ReadRaw, ReadNext)
- https://raw.githubusercontent.com/pmachapman/unrar/master/qopen.hpp (MaxBufSize = 0x10000, struct QuickOpenItem, class QuickOpen)
- https://raw.githubusercontent.com/pmachapman/unrar/master/volume.cpp (MergeArchive, lines 10-193)
- https://raw.githubusercontent.com/pmachapman/unrar/master/arcread.cpp (ReadHeader15 split flags 257-258; ReadHeader50 QO hook 788-805; HFL split flags 867-868; ProcessExtra50 locator 1012-1020)
- https://raw.githubusercontent.com/pmachapman/unrar/master/archive.cpp (Archive::Open, Read, Seek, Tell)
- https://raw.githubusercontent.com/pmachapman/unrar/master/cmddata.cpp (CommandData::Init lines 12-18; -ds ~406; -ep ~430-445; -qo ~823-838; -r ~843-856; -qo incompatibility ~1287)
- https://raw.githubusercontent.com/pmachapman/unrar/master/options.hpp (PATH_EXCL_MODE 10-15; QOPEN_MODE 72; RECURSE_MODE 48-52; QOpenMode 137-138; DisableSortSolid 179)
- https://raw.githubusercontent.com/pmachapman/unrar/master/options.cpp (QOpenMode=QOPEN_AUTO, line 21)
- https://raw.githubusercontent.com/pmachapman/unrar/master/headers.hpp (LHD_SPLIT_* 39-40; SUBHEAD_TYPE_QOPEN 119; QOpenOffset 179)
- https://raw.githubusercontent.com/pmachapman/unrar/master/headers5.hpp (HFL_SPLITBEFORE/AFTER 16-18; MHEXTRA_LOCATOR 68; MHEXTRA_LOCATOR_QLIST 72)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zUpdate.cpp (CompareEmptyItems 703; GetExtIndex ~755; CRefItem::Construct ~790; CompareUpdateItems 850-918; isSolid 2061; sort 2706-2717; dirs 2250-2253; empty files 2262-2284; Files.Add 2734; solid sub-files 2745)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zUpdate.h (UseTypeSorting default true, line 142)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp (InitProps7z 856-873; InitProps 879-883; SetOptions 800; property "qs" 1044)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zOut.cpp (WriteDatabase 883-930; WriteStartHeader 68; signature 108-130)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/UI/Common/SortUtils.cpp (SortFileNames)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/UI/Common/UpdatePair.cpp (directory names sorted with SortFileNames, about line 180)
- https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/Zip/ZipUpdate.cpp (grep for sort: no match)
- https://raw.githubusercontent.com/golang/go/master/src/archive/zip/writer.go (CreateHeader ~292-400; Close 82-97)
- Not available: https://raw.githubusercontent.com/rarlab/rar/master/rar.txt (404); rarlab.com (blocked)
