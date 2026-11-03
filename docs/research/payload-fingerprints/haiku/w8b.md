# W8b: PAR2 defaults across posting tools (exp-0 recovery-slice key)

Scope: source, docs and man pages fetched 2026-11-02. Items not confirmed from source are marked UNVERIFIED. Fetch count: about 16 WebFetch, 5 WebSearch.

## Table

| Tool | Default slice size | Default redundancy | First exponent | Field/base compatible with par2cmdline | Source URL |
|---|---|---|---|---|---|
| par2cmdline (par2create) | Block count `-b` defaults to 2000 when neither `-b` nor `-s` is given. The `ceil(total/2000)` rounded-to-4 rule is NOT on this page (UNVERIFIED). | `-r` default 5% | `-f` default 0 ("First Recovery-Block-Number (default 0)") | Reference implementation (GF(2^16), 0x1100B, per PAR2 spec; not re-checked here) | https://man.archlinux.org/man/extra/par2cmdline/par2create.1.en |
| ParPar 0.4.5 | No default. `-s` "must be specified" (help text). | `-r` default 0 ("Default is 0, which generates no recovery.") | `-e/--recovery-offset` "Default 0". lib/par2gen.js: `recoveryOffset: 0,` | GF16 poly and base NOT verified (native `parpar_gf`, not in fetched source). `--recovery-exponents` lets the user choose exponents. | https://app.unpkg.com/@animetosho/parpar@0.4.5/files/help-full.txt ; https://cdn.jsdelivr.net/npm/@animetosho/parpar@0.4.5/lib/par2gen.js |
| par2cmdline-turbo | UNVERIFIED (README has no default) | UNVERIFIED | UNVERIFIED (README: "aims to keep close with its upstream and only differs in the following areas related to performance") | Claims drop-in replacement; GF16/MD5/CRC32 use ParPar's implementation. Byte-identical output NOT verified. | https://github.com/animetosho/par2cmdline-turbo |
| MultiPar / par2j | Block size "auto-calculated by default" (secondary summary only). Block-count rule UNVERIFIED. | UNVERIFIED (secondary page gives 10-15% as a recommendation, not a default) | UNVERIFIED (`/rd`, `/fr` not documented on fetched pages) | UNVERIFIED. MultiPar readme fetch failed; GitHub page has no encoding parameters. | https://skills.cat/skills/doiiarx/claude-skills/multipar-cli (secondary) ; https://github.com/Yutaka-Sawada/MultiPar (no params) |
| QuickPar | UNVERIFIED (search snippet only; the doc page was robots-blocked) | UNVERIFIED | UNVERIFIED | UNVERIFIED | Search snippet only: discmaster QuickPar 0.9.1 "CreatingPAR2Files" (not fetched) |
| ngPost | Not set in ngPost. README: "0 by default meaning NO par2 generation" for `--par2_pct`. | `--par2_pct` default 0 (README). `PAR2_PCT = 8` in the shipped ngPost.conf sample. | Not set (delegated to the tool) | Uses embedded par2cmdline by default; ParPar/MultiPar only via `PAR2_CMD`/`PAR2_ARGS`. | https://raw.githubusercontent.com/mbruel/ngPost/master/README.md ; https://raw.githubusercontent.com/mbruel/ngPost/master/ngPost.conf |
| Nyuu | No PAR2 in fetched page. Planned: "Integrate ParPar for streaming PAR2 creation." | n/a | n/a | n/a | https://github.com/animetosho/Nyuu |
| Postie, JBinUp, GoPostStuff, newsup, PowerPost, Newsleecher | UNVERIFIED (not fetched) | UNVERIFIED | UNVERIFIED | UNVERIFIED | none fetched |

ngPost.conf commented PAR2_ARGS examples (sample config, not source defaults): `-s5M -r1n*0.6 -m2048M -p1l --progress stdout -q` (ParPar); `c -l -m1024 -r8 -s768000` (par2cmdline); `create /rr8 /lc40 /lr /rd2 /ss768000` (MultiPar). Two of three use a fixed slice constant.

## Proves / Cost

- Proves: if two posts carry the same exponent-0 recovery slice, they encode the same input bytes (same slice size, zero-padded final slice, same slice order is irrelevant for XOR). Exponent 0 gives coefficient 1 in any field, so the poly/base question does not matter for this key. It matters for exponents other than 0.
- Cost: read the first recovery packet of the smallest PAR2 recovery file. Its body is one slice (slice size plus 64-byte packet header), so about 1 to a few articles. The exponent is in the packet, at byte offset 64 per ParPar's writer (`pkt.writeUInt32LE(num, 64)`, lib/par2.js).
- Pair count: verified exp-0 defaults are par2cmdline and ParPar, so 1 pair (par2cmdline vs ParPar), and only when slice size matches. Turbo would add up to 2 pairs (UNVERIFIED). MultiPar and QuickPar are UNVERIFIED. Upper bound is 10 pairs among 5 tools, if all five start at exponent 0.

## Takeaways

1. ParPar and par2cmdline both default to exponent 0 (verified), so their exp-0 slices can match when slice sizes agree.
2. ParPar has no slice-size default (`-s` is required), so a fixed `-s` constant is the only way ParPar matches a known size.
3. ngPost does not default to ParPar; it uses embedded par2cmdline and only generates PAR2 when `--par2_pct` is set.
4. The ngPost sample config uses fixed slice constants (`-s5M`, `-s768000`, `/ss768000`), which suggests fixed sizes are common among ngPost users (not measured).
5. Turbo, MultiPar, QuickPar and the other posters are UNVERIFIED. Test a byte-identical exp-0 slice on a known pair before using the key.
