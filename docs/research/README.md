# Research archive

Research done on 2026-11-02 into what metadata Usenet posts carry and how Atlas could use it to describe releases and link twins, alts and split posts. Most of it is not built yet. **What is left to build is listed in [TODO.md](TODO.md).**

Everything here is a snapshot: the notes cite Atlas code at the commit they checked (mostly `a29ee52`, `ad111a0` or `55d2e51`), so recheck line numbers before you build from them. Tools and prototypes (Python, Rust, C, JS) are research code, not part of the Atlas build.

## Start here

| Read | What it is |
| --- | --- |
| [TODO.md](TODO.md) | Every finding not yet acted on, grouped by area, with the file that explains it |
| [linking-findings.md](linking-findings.md) | Shared notebook every research thread appended to, wave by wave |
| [visuals/linking-signals.html](visuals/linking-signals.html) | Linking Signals Map: 37 signals by link type, read cost and Atlas status (open in a browser) |
| [prior-art/synthesis.md](prior-art/synthesis.md) | What other indexers, tools, specs and forums already do, with sources |

## Topics

| Folder | Question | Final report | Field catalog |
| --- | --- | --- | --- |
| [archive-metadata/](archive-metadata/) | What RAR4/RAR5, 7z, zip, PAR2, SFV, SRR headers hold, and how to read inside compressed archives | [FINAL-REPORT.md](archive-metadata/FINAL-REPORT.md) | [archive-field-catalog.yaml](archive-metadata/archive-field-catalog.yaml) (267 fields) |
| [container-metadata/](container-metadata/) | What MKV, MP4, ISO/UDF, Blu-ray, DVD, TS and AVI headers hold | [FINAL-REPORT.md](container-metadata/FINAL-REPORT.md) | [catalog/](container-metadata/catalog/) (mkv, mp4, disc_other, additions, signals) |
| [stream-metadata/](stream-metadata/) | Bitstream facts: SPS/VUI, HDR10 SEI, HDR10+, Dolby Vision RPU, Atmos, DTS:X, frame and audio fingerprints | [FINAL-REPORT.md](stream-metadata/FINAL-REPORT.md) | [catalog.yaml](stream-metadata/catalog.yaml) |
| [payload-fingerprints/](payload-fingerprints/) | Which payload hashes (yEnc pcrc32, PAR2, inner CRCs, sizes) prove a twin or an alt | [final-report.md](payload-fingerprints/final-report.md) | [final-inventory-draft.md](payload-fingerprints/final-inventory-draft.md) |
| [posting-behavior/](posting-behavior/) | Linking split and obfuscated posts by timing, posting order and server IDs | [FINAL-REPORT.md](posting-behavior/FINAL-REPORT.md) | wave files; pipeline spec in [wave-09-A-pipeline-spec.md](posting-behavior/wave-09-A-pipeline-spec.md) |
| [yenc-header-forensics/](yenc-header-forensics/) | Posting-tool fingerprints from yEnc and NNTP headers, Message-ID generators (Nyuu solver) | [FINAL-REPORT.md](yenc-header-forensics/FINAL-REPORT.md) and [opus/FINAL-yenc-header-forensics.md](yenc-header-forensics/opus/FINAL-yenc-header-forensics.md) | build spec [opus/w9b-atlas-rust-spec.md](yenc-header-forensics/opus/w9b-atlas-rust-spec.md) |
| [metadata-visibility/](metadata-visibility/) | Code map for showing every catalog field on the release page (partly built in #65 and #67) | [MAP.md](metadata-visibility/MAP.md) | section 4 of MAP.md |
| [prior-art/](prior-art/) | Survey of existing tools and sources | [synthesis.md](prior-art/synthesis.md) | [findings.json](prior-art/findings.json) |

Each topic folder keeps its `log.md` (wave log), `raw/` notes per wave, and the tools used. The three `FINAL-REPORT.md` files for archives, containers and streams are Markdown exports of the Claude Docs linked at their top.

## Notes

- `posting-behavior/server-ids/segs.tsv.gz` is gzipped (8 MB raw): server-ID samples from public NZBs. `zcat` it to use the scripts beside it.
- Archive-password recovery (for example via the ngPost seed) was kept out of scope on purpose.
- Privacy fields the reports flag (iTunes `apID`/`purd`/`ownr`, Matroska `ENCODED_BY`, ISO preparer, RAR5 Unix owner) must never be displayed; store a salted hash at most.
