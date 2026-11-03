# Wave 1 / Agent A: yEnc encoder quirks (Haiku)
- Escape set differs per encoder: spec 1.3 requires NUL LF CR '='; optional TAB/SPACE at line ends, leading '.'. yencoding (Rust) escapes TAB and '.' only at line start, not space. newsmangler yenc.py escapes TAB/SPACE at start/end and leading '.', and pulls the next char onto a line ending in '='. Read the first/last body lines. (3/5)
- Line length: node-yencode default 128. A line is n+1 long when it ends in an escape. Also check for a short final line. (2/5)
- CRC hex case and padding in =yend (newsmangler uses lowercase, 8 digits zero padded). (3/5)
- Field layout: total= present or absent, field order, crc32= on every part or only the last; yencoding accepts a tab before name=. (3/5)
- CRLF vs LF, a blank line before =yend. (2/5)
- =ypart begin starts at 1 or 0, and whether end = begin+size-1 holds; sibling parts chain by arithmetic. (2/5)
- name= shape and quoting. (2/5)
Leads: diff the same file encoded by two encoders; duplicate-key handling; Nyuu JS wrapper header strings.
Sources: yence.sourceforge.net/docs/protocol/version1_3_draft.html, docs.rs/crate/yencoding/0.2.0, github.com/madcowfred/newsmangler yenc.py, github.com/animetosho/node-yencode src/yencode.cc
