# Wave 1A: yEnc encoder quirks (Opus subagent)
S1 header templates: Nyuu `=ybegin part total line size name` / `=ypart begin end` / `=yend size part pcrc32` (+crc32 last part only), always multipart even for 1 part (Nyuu lib/article.js). ngPost: `=yend size pcrc32` NO part=, NO crc32= ever, line=128 hardcoded (ngPost src/nntp/NntpArticle.cpp). newsmangler: line=128, yend size part pcrc32, no final crc32 (newsmangler/article.py). Rust yenc crate: no total= (1.1 style), {:08x}.
S2 ngPost pcrc32 printed std::hex without zero padding -> ~1/16 parts have <8 hex digits. Nyuu/newsmangler/Rust always 8 lowercase.
S3 line-end whitespace: RapidYenc/node-yencode escapes TAB/SPACE at last column (not '.'); ngPost test `column-1==maxwidth` never true -> never escapes trailing TAB/SPACE (ngPost src/utils/Yenc.cpp); Rust crate never escapes TAB/SPACE; newsmangler escapes both ends. ~2/256 lines vote.
S4 leading dot: RapidYenc & ngPost "=n"; newsmangler(python yenc)/Rust crate dot-stuff -> raw '.' after unstuffing.
S5 line lengths: line or line+1 when last char escaped; non-128 line= is a config marker; line-length histogram identical for same-tool twins (inference).
S6 ngPost/Rust crate: empty line before =yend when part length is exact multiple of 128.
S7 node-yencode issue #3: old builds failed to escape critical char at last byte of segment -> Nyuu version fingerprint. https://github.com/animetosho/node-yencode/issues/3
S8 name= truncation/encoding (Nyuu strips RE_BADCHAR, MAX_NAME_LENGTH).
S9 spec examples have trailing space; old Windows tools may copy (unverified).
S10 pcrc32 over raw data: (size,total,begin/end list,pcrc32 list,crc32) identical across tools for same split; body hash equality => same tool+settings; size= + part1 pcrc32 ~ unique file id from one article.
Most obscure: ngPost signature from body bytes alone (unpadded pcrc32 + unescaped trailing ws + no part=/crc32= + blank line before =yend).
Leads: old Windows posters (PowerPost, yEnc32, JBinUp, Camelsystem, NewsUP Perl, GoPostStuff); Nyuu/node-yencode version timeline; empirical clustering validation against real articles.
Sources: yenc-draft.1.3 (caronc/newsreap docs), node-yencode encoder.cc, Nyuu article.js, ngPost NntpArticle.cpp+Yenc.cpp, newsmangler yenc.py/article.py, docs.rs yenc encode.rs, sabyenc yenc.cc.
