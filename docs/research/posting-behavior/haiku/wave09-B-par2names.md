# Wave 09-B: Which name lands in the PAR2 FileDesc packet when obfuscation is on

Scope: Postie (confirmed in source), ngPost, ParPar, par2cmdline, Nyuu, NewsUP, nxg-upper, SABnzbd, NZBGet.
Sources cloned with `git clone --depth 1` to /tmp/w9b/. Line numbers are from those clones at HEAD.
Tags: **verified in source** (read the code), **docs** (README/help text), **guess** (inferred, not run).

## Correction to the prior agent's claim

- Postie does **not** rename files on disk for PAR2. The only obfuscation is at the article level: the yEnc name and the subject (see Part 1). The PAR2 packets are built from the real names.
- `internal/par2/par2.go:107-113` is the **existing-PAR2 reuse check** (`checkExistingPar2FilesInPath`), not packet creation. It compares an on-disk PAR2 against the real `baseName`. The real creation path is `CreateSet` at `par2.go:223-259` and `par2.go:340`.

## Part 1: Postie (verified in source, Go, github.com/javi11/postie)

**FileDesc name source.** `CreateSet` builds its inputs with `setInputNames(inputs, folderDir)` (`internal/par2/par2.go:259`). That function is in `internal/par2/existing.go:233-243`:

```go
name, err := filepath.Rel(folderDir, f.Path)
if err != nil || name == "" || name == "." {
    name = filepath.Base(f.Path)
}
out[i] = par2go.InputFile{Path: f.Path, Name: filepath.ToSlash(name)}
```

So `Name`, which is the FileDesc name, is the **real on-disk path relative to the posted folder**. It is never the obfuscated name. The set is written with `par2go.CreateWithNames(ctx, par2Path, par2Inputs, opts)` (`par2.go:340`).

The per-file path (`par2.go:450-451`, `par2.go:505`) names the PAR2 file `filepath.Base(file.Path) + ".par2"` and calls `par2go.Create(ctx, par2Path, []string{file.Path}, opts)`. This does not pass an explicit `Name`, so the FileDesc name comes from the par2go default. The default is likely the basename, which is a **guess**; par2go v0.0.14 is in `go.mod:17` and was not read.

**Call sites and timing.** `pkg/postie/postie.go:718` and `:788` call `p.par2runner.CreateSet(ctx, files, par2OutputDir, folderName, folderDir)` in the preparation step. The comment at `postie.go:716-717` says FileDesc paths are "computed relative to it so SABnzbd recreates the tree inside the job folder". Verified from the call sites. Ordering relative to posting is inferred (the call precedes article upload), not traced end to end.

**Obfuscated yEnc name (differs from FileDesc).** `internal/poster/poster.go:1436-1452`:
- `fileName := filepath.Base(filePath)` (line 1436)
- `obfuscationPolicy` is `p.cfg.Par2ObfuscationPolicy` when `par2.IsPar2File(filePath)` (lines 1439-1441), otherwise `p.cfg.ObfuscationPolicy`
- `ObfuscationPolicyNone`: `fName = fileName` (1444-1445)
- `ObfuscationPolicyFull`: `fName = article.GenerateRandomFilename()`; subject is random (1446-1448)
- default: `fName = md5(fileName + partNumber)` (1449-1452), so the yEnc name is a hash and the subject is obfuscated (1456-1460)
- `fName` is passed as the yEnc name at `poster.go:1507`

`internal/config/config.go:1122` sets `Par2ObfuscationPolicy: ObfuscationPolicyFull` in a default config (verified in source; the surrounding config context was not read).

**Result, obfuscation on (any policy): the FileDesc name is REAL.** The yEnc name is obfuscated, so the two names differ. The leak is confirmed in source.

## Part 2: Other tools

### ngPost (C++/Qt, github.com/mbruel/ngPost)
- **Compressed path, `--compress --gen_name` (obfuscate filename):** `src/PostingJob.cpp:991-1002` renames each original to a random name (`_ngPost->randomPass(_ngPost->_lengthName)`) before `startCompressFiles` (line 902). The originals are renamed back after compression at `PostingJob.cpp:1076-1079`. PAR2 runs afterwards (`startGenPar2`, lines 1091-1095 and 1113). It targets the archive folder and archive name (`archiveTmpFolder = tmpFolder/archiveName`, line 1128-1134), where `archiveName = _rarName`, which is random with `--gen_name`. Because PAR2 is built over the .rar volumes (random names), **FileDesc = OBFUSCATED**. Tag: verified in source.
- **Non-compressed `--gen_par2`:** `PostingJob.cpp:331-337` builds the list from `_files` (originals). The rename at 991 is inside `startCompressFiles`, so it does not run here. **FileDesc = REAL.** Tag: verified in source.
- PAR2 tool: `startGenPar2` shells out to par2cmdline (`c -l -m1024 -r...`) or ParPar (`-o`, `-R archiveTmpFolder`), per `PostingJob.cpp:1121-1139`. Tag: verified.
- Edge case: ngPost passes absolute paths with no `-B` base, so par2cmdline would record the path as passed (see below). Tag: guess.

### ParPar (JS wrapper and CLI, github.com/animetosho/ParPar)
- `--filepath-format` is `basename | keep | common | outrel | path`, with default `common` (`help.txt:32-44`; `bin/parpar.js:102-108`). `basename` discards paths. `common` strips the common prefix and falls back to basename when there is none. Tag: docs and verified in source.
- `lib/par2gen.js:268` sets `displayNameFormat: 'common'` as default. The mode switch is at `lib/par2gen.js:833-883`, which sets `file.displayName` for each file. Tag: verified in source (JS wrapper). The C++ core is not in the repo.
- ParPar performs no renaming. It records whatever names it is given, so FileDesc is **real** when given real names, and **obfuscated** when given renamed files (the ngPost compressed path).

### par2cmdline (C++, github.com/Parchive/par2cmdline)
- `src/par2creatorsourcefile.cpp:70`: `parfilename.erase(0, basepath.length())`, then the name is translated to PAR2 form. The FileDesc name is the path with the `-B` basepath prefix removed. Tag: verified in source.
- `src/cli.cpp:683`: `Par2Creator creator(commandline.GetBasePath(), backends)`. The default basepath is empty (`src/cli.cpp:91`, `basepath()`). So with no `-B`, the stored name is the full path as passed. Tag: verified in source for the mechanism; the default outcome is a **guess** until run.

### Nyuu (Node, github.com/animetosho/Nyuu)
- No PAR2 generation found. The only `par2` references are filename regexes for grouping an existing `.par2` (`lib/fileuploader.js:123`, `bin/nyuu.js:251`) and `config.js:164`. **Nyuu does not create PAR2 packets**, so there is no FileDesc to leak. Tag: verified in source (grep).

### NewsUP (github.com/...): NOT VERIFIED
- The repo was not cloned. Candidate owners tried and failed: `Ranger/NewsUP`, `kuschku/newsup`, `newsup/newsup`, `Mayoff/NewsUP`, `demel/NewsUP`. The README `par2 rename` option (~line 207) was not read. Tag: not checked. The owner needs confirmation before this row can be filled.

### nxg-upper: NOT VERIFIED
- Not cloned. Candidates `nxg-upper/nxg-upper` and `nxgupper/nxg-upper` failed. Tag: not checked.

## Part 3: Downloaders use the FileDesc name to deobfuscate (this proves the leak is exploited)

### SABnzbd (Python, github.com/sabnzbd/sabnzbd)
- `sabnzbd/postproc.py:579-588`: after a verified job, `deobfuscate.recover_par2_names(newfiles)` runs when `nzo.delete and cfg.process_unpacked_par2()`. Later, `deobfuscate.deobfuscate(nzo, newfiles, nzo.final_name)` runs.
- `sabnzbd/deobfuscate_filenames.py:22-23` (docstring): it checks the job folder for par2 files, "for example rename.par2", and uses them to rename files.
- `deobfuscate_filenames.py:48-57`: `decode_par2` parses the PAR2 with `parse_par2_file`, which reads FileDesc packets.
- `deobfuscate_filenames.py:75-79`: the par2-provided name is treated as untrusted (contained, and skipped on path traversal) before rename.
- `deobfuscate_filenames.py:85-105`: `recover_par2_names` runs the par2 rename over every par2 file in the job.
- Tag: verified in source.

### NZBGet (C++, github.com/nzbget/nzbget)
- `daemon/postprocess/ParRenamer.cpp:115-118` and `:125-144`: loads par2 files from the destination directory.
- `ParRenamer.cpp:180`: `filename = Par2::DiskFile::TranslateFilename(sourceFile->GetDescriptionPacket()->FileName())`. This is the FileDesc name.
- `ParRenamer.cpp:192-193`: `m_fileHashList.emplace_back(filename, hash)` and `RegisterParredFile(filename)`. The FileDesc name is matched to a file by its 16K MD5 hash and then renamed.
- Tag: verified in source.

## Part 4: Bottom line (PAR2 FileDesc name when obfuscation is on)

| Tool | Mode | FileDesc name | Source | Tag |
|---|---|---|---|---|
| Postie | any obfuscation policy, incl. Par2ObfuscationPolicy | **REAL** (folder-relative path) | par2.go:259, existing.go:233-243, postie.go:718/788; yEnc obfuscated at poster.go:1436-1452 | verified |
| ngPost | `--compress --gen_name` | **OBFUSCATED** (random .rar volume names) | PostingJob.cpp:991-1002, 1076-1079, 1113-1134 | verified |
| ngPost | `--gen_par2` without compress | **REAL** | PostingJob.cpp:331-337 | verified |
| ParPar | depends on inputs; default `common` | mirrors input names (real if input real) | par2gen.js:268, 833-883; help.txt:32-44 | verified (JS wrapper) |
| par2cmdline | path as passed, minus `-B` | mirrors input path | par2creatorsourcefile.cpp:70; cli.cpp:683, 91 | verified mechanism; default outcome guess |
| Nyuu | no PAR2 creation | n/a | grep: lib/fileuploader.js:123, bin/nyuu.js:251 | verified |
| NewsUP | par2 rename option | unknown | not cloned | not checked |
| nxg-upper | unknown | unknown | not cloned | not checked |

**Timing (PAR2 before or after rename):**
- Postie: PAR2 is built in the prep step from real names (postie.go:718/788). No on-disk rename occurs. Obfuscation is applied only to the article yEnc name and subject. Ordering is inferred.
- ngPost compressed: rename happens **before** rar, and rename-back happens **before** PAR2 (PostingJob.cpp:991 -> 1076 -> 1091). So PAR2 sees the obfuscated archive names.
- ngPost non-compressed: no rename before PAR2, so PAR2 sees real names (PostingJob.cpp:331-337).

## What the indexer should do

- Parse the FileDesc packets from the PAR2 main file and treat their names as a real-name hint. When the yEnc name is hash-like (md5 hex, UUID, or random), use the FileDesc name for deobfuscation, as SABnzbd and NZBGet do.
- Link PAR2 sets to data files by the FileDesc 16K MD5 and file length, not by name. This works whether the yEnc name is obfuscated or not, which covers the ngPost compressed case, where the names are random.
- Flag a post where the FileDesc name differs from the yEnc name. That is the Postie signature, and it means the poster's PAR2 is leaking the real name.
