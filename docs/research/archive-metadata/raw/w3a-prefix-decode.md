# W3a: Prefix decode of compressed archive members (pure-Rust decoders)

Question: given only the first P% of a compressed member (first N compressed bytes), how many bytes of decompressed output can each pure-Rust decoder produce before failing, and is that output a correct prefix of the original?

## Method
- Input: `input.bin`, 8,388,608 bytes. A fake EBML/MKV header, then alternating 4,000 B repeated English text and 64 KiB of `os.urandom` (random data is realistic for video payload; text portions compress well).
- Fixtures (all in `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w3a/fx/`):
  - Raw deflate (zlib level 9, raw, no zlib/gzip header): 7,925,520 B. Same payload class as `zip -9`. (zip itself was made, `input.zip`, but the member bytes were taken from zlib directly.)
  - LZMA1 raw stream, liblzma FORMAT_RAW, preset 6 (props byte 0x5D = lc3 lp0 pb2, dict 8 MiB): 8,012,743 B. Same bitstream class as 7z coder 030101.
  - LZMA2 raw stream, liblzma FORMAT_RAW, preset 6 (dict 8 MiB): 8,007,508 B. Same as 7z coder 21.
  - A real `input_lzma2.7z` was also written by py7zr 1.1.4 (8,007,654 B) but was NOT used in the probe: the packed stream sits at offset 32 and the props live in the header at the end of the file, so a prefix test on the .7z itself needs header parsing that was out of scope for the 10-minute cap.
  - RAR fixtures from the rars 0.12.3 crate `tests/fixtures` (the .crate tarball ships them): `farmanager170.rar` (RAR3 PPMd, 996,166 B, 3,343,305 B decoded), `doc_154_best.rar` (RAR 1.54, 189,390 B, 502,961 B decoded), `m3_default.rar` (RAR5, 10,007 B, 65,536 B decoded), `unpack20_multiblock.rar` (RAR 2.0 / unpack20, 4,827 B, 16,384 B decoded). No rar binary was available. Reference for RAR = full decode of the untruncated file.
- Truncation: first P% of the compressed bytes. For the raw streams that is exactly the coder input. For RAR it is the whole file truncated at P% (headers are a few hundred bytes, so this is about the packed data).
- Output counting: read loop with a 64 KiB buffer until Ok(0) or Err. For RAR, bytes are counted through a shared `Write` sink, so output written before an error is kept. Correct prefix = produced bytes equal the first K bytes of the original (or of the full-decode reference).

## Results

| codec | crate | truncation % | compressed bytes fed | output bytes | correct prefix? | terminal condition |
|---|---|---|---|---|---|---|
| deflate | flate2 1.1.10 | 10 | 792552 | 842356 | yes | UnexpectedEof: incomplete deflate stream |
| deflate | flate2 1.1.10 | 25 | 1981380 | 2100061 | yes | UnexpectedEof: incomplete deflate stream |
| deflate | flate2 1.1.10 | 50 | 3962760 | 4196251 | yes | UnexpectedEof: incomplete deflate stream |
| deflate | flate2 1.1.10 | 100 | 7925520 | 8388608 | yes | EOF |
| LZMA1 | lzma-rust2 0.25.0 | 10 | 801274 | 786432 | yes | UnexpectedEof: unexpected end of LZMA input |
| LZMA1 | lzma-rust2 0.25.0 | 25 | 2003185 | 2097152 | yes | UnexpectedEof: unexpected end of LZMA input |
| LZMA1 | lzma-rust2 0.25.0 | 50 | 4006371 | 4194304 | yes | UnexpectedEof: unexpected end of LZMA input |
| LZMA1 | lzma-rust2 0.25.0 | 100 | 8012743 | 8388608 | yes | EOF |
| LZMA2 | lzma-rust2 0.25.0 | 10 | 800750 | 786432 | yes | UnexpectedEof: failed to fill whole buffer |
| LZMA2 | lzma-rust2 0.25.0 | 25 | 2001877 | 2031616 | yes | UnexpectedEof: failed to fill whole buffer |
| LZMA2 | lzma-rust2 0.25.0 | 50 | 4003754 | 4128768 | yes | UnexpectedEof: failed to fill whole buffer |
| LZMA2 | lzma-rust2 0.25.0 | 100 | 8007508 | 8388608 | yes | EOF |
| RAR3 PPMd farmanager170.rar | rars 0.12.3 | 10 | 99616 | 0 | yes | input is too short |
| RAR3 PPMd farmanager170.rar | rars 0.12.3 | 25 | 249041 | 0 | yes | input is too short |
| RAR3 PPMd farmanager170.rar | rars 0.12.3 | 50 | 498083 | 0 | yes | input is too short |
| RAR3 PPMd farmanager170.rar | rars 0.12.3 | 100 | 996166 | 3343305 | yes | EOF |
| RAR5 m3_default.rar | rars 0.12.3 | 10 | 1000 | 0 | yes | at archive offset 0x19: input is too short |
| RAR5 m3_default.rar | rars 0.12.3 | 25 | 2501 | 0 | yes | at archive offset 0x19: input is too short |
| RAR5 m3_default.rar | rars 0.12.3 | 50 | 5003 | 0 | yes | at archive offset 0x19: input is too short |
| RAR5 m3_default.rar | rars 0.12.3 | 100 | 10007 | 65536 | yes | EOF |
| RAR1.54 doc_154_best.rar | rars 0.12.3 | 10 | 18939 | 0 | yes | input is too short |
| RAR1.54 doc_154_best.rar | rars 0.12.3 | 25 | 47347 | 0 | yes | input is too short |
| RAR1.54 doc_154_best.rar | rars 0.12.3 | 50 | 94695 | 0 | yes | input is too short |
| RAR1.54 doc_154_best.rar | rars 0.12.3 | 100 | 189390 | 502961 | yes | EOF |
| RAR3 unpack20_multiblock.rar | rars 0.12.3 | 10 | 482 | 0 | yes | input is too short |
| RAR3 unpack20_multiblock.rar | rars 0.12.3 | 25 | 1206 | 0 | yes | input is too short |
| RAR3 unpack20_multiblock.rar | rars 0.12.3 | 50 | 2413 | 0 | yes | input is too short |
| RAR3 unpack20_multiblock.rar | rars 0.12.3 | 100 | 4827 | 16384 | yes | EOF |

Notes on the table:
- Deflate, LZMA1, LZMA2 at 10% produce 0.79-0.84 MB: roughly 10% of 8 MiB plus a bit of dictionary/buffer slack. Output is correct in all rows.
- LZMA1 and LZMA2 output counts are multiples of 256 KiB (786,432 = 3 x 256 KiB; 2,097,152 = 8 x 256 KiB at 25%). lzma-rust2 appears to hand out output in dictionary-flush chunks, so the usable prefix is coarser than the input cut. Not verified in source.
- The input is 94% random bytes, so compressed size is close to raw size. For text-heavy data (typical of subtitles, NFOs, or the first MBs of an MKV with a lot of header/metadata), the output per compressed byte would be much larger. These numbers are a lower bound for text and about right for video payload.
- rars 0.12.3 (RAR3, RAR 1.54, RAR5) returned 0 bytes for every truncated case with the error `input is too short` (or `at archive offset 0x19: input is too short` for RAR5). rars checks that the file is full length during parse (header or packed-size validation), before any decoding. So as-is, rars cannot decode a truncated RAR. This is a parse-time gate, not a codec limit. It was NOT tested whether padding the truncated file with zeros up to full length (so the packed-size check passes) lets rars emit the correct prefix. That is the obvious next experiment. Output past the cut would be garbage and must be discarded.
- Wall-clock limit: the 10-minute cap was reached. The padding experiment and the sevenz-rust2 test were not run.

## Crate versions and API
- flate2 1.1.10: `flate2::read::DeflateDecoder::new(reader)`.
- lzma-rust2 0.25.0 (0.23.0 also pulled in by a transitive dependency): `lzma_rust2::LzmaReader::new(reader, uncomp_size, lc, lp, pb, dict_size, preset_dict: Option<&[u8]>) -> Result<LzmaReader>`; `lzma_rust2::Lzma2Reader::new(inner, dict_size, preset_dict)`.
- sevenz-rust2 0.24.2: pulled in and resolved, but NOT used. Its per-coder decoder (`decoder::add_decoder`) is not exported as a public path that was checked. The archive-level entry point needs a real .7z.
- rars 0.12.3: `rars::ArchiveReader::read_reader(Cursor<Vec<u8>>) -> Result<Archive>`, then `archive.extract_to(None, |meta: &ExtractedEntryMeta| -> Result<Box<dyn Write>>)`.
- Build: `cargo build --release`, Rust 1.97.0. Cargo.lock: flate2 1.1.10, lzma-rust2 0.25.0, sevenz-rust2 0.24.2, rars 0.12.3.
- Fixture generation: Python 3.13.18 `lzma` and `zlib` modules, `py7zr` 1.1.4 (pip).

## Key code snippet

```rust
// LZMA1 (7z coder 030101): props lc=3 lp=0 pb=2, dict 8 MiB, known size
let r = lzma_rust2::LzmaReader::new(cursor, LEN, 3, 0, 2, DICT, None)?;
// LZMA2 (7z coder 21)
let r = lzma_rust2::Lzma2Reader::new(cursor, DICT, None);
// deflate
let r = flate2::read::DeflateDecoder::new(cursor);
// rars: parse the (truncated) file, then stream members to a sink
let arc = rars::ArchiveReader::read_reader(Cursor::new(bytes))?;
arc.extract_to(None, move |_meta| Ok(Box::new(Sink(buf.clone())) as Box<dyn Write>))?;
```

## Source: `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w3a/prefixdec/src/main.rs`

```rust
// Prefix-decode probe: feed the first P% of a compressed stream/archive to a
// pure-Rust decoder and count how many decoded bytes come out before error/EOF.
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};

const FX: &str = "/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w3a/fx";
const PCTS: [u32; 4] = [10, 25, 50, 100];
const DICT: u32 = 8 * 1024 * 1024; // preset 6 dictionary (8 MiB)
const LEN: u64 = 8 * 1024 * 1024; // input.bin size

struct Row {
    codec: &'static str,
    crate_name: &'static str,
    pct: u32,
    take: usize,
    out: usize,
    prefix_ok: bool,
    err: String,
}

fn drain<R: Read>(mut r: R) -> (Vec<u8>, String) {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return (out, "EOF".into()),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => return (out, format!("{:?}: {}", e.kind(), e)),
        }
    }
}

fn run_raw(
    codec: &'static str,
    crate_name: &'static str,
    comp: &[u8],
    src: &[u8],
    dec: impl Fn(Cursor<Vec<u8>>) -> Box<dyn Read>,
    rows: &mut Vec<Row>,
) {
    for pct in PCTS {
        let take = comp.len() * pct as usize / 100;
        let input = Cursor::new(comp[..take].to_vec());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drain(dec(input))));
        let (out, err) = match r {
            Ok(v) => v,
            Err(_) => (Vec::new(), "PANIC".into()),
        };
        let prefix_ok = out.len() <= src.len() && src[..out.len()] == out[..];
        rows.push(Row { codec, crate_name, pct, take, out: out.len(), prefix_ok, err });
    }
}

// Writer that appends to a shared buffer, so partial output survives an error.
struct Sink(Arc<Mutex<Vec<u8>>>);
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn rar_decode(bytes: Vec<u8>) -> (Vec<u8>, String) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let res = rars::ArchiveReader::read_reader(Cursor::new(bytes))
        .and_then(|a| {
            let b = buf.clone();
            a.extract_to(None, move |_meta| Ok(Box::new(Sink(b.clone())) as Box<dyn Write>))
        });
    let out = buf.lock().unwrap().clone();
    let err = match res { Ok(()) => "EOF".to_string(), Err(e) => format!("{e}") };
    (out, err)
}

fn main() {
    std::panic::set_hook(Box::new(|_| {})); // keep panics from flooding output
    let src = std::fs::read(format!("{FX}/input.bin")).unwrap();
    assert_eq!(src.len() as u64, LEN);
    let mut rows: Vec<Row> = Vec::new();

    // 1. deflate (raw deflate stream, zlib level 9; same payload class as zip -9)
    let comp = std::fs::read(format!("{FX}/input.deflate.raw")).unwrap();
    run_raw("deflate", "flate2 1.1.10", &comp, &src, |c| Box::new(flate2::read::DeflateDecoder::new(c)), &mut rows);

    // 2. LZMA1 raw stream (7z coder 030101), props lc=3 lp=0 pb=2 (byte 0x5D), dict 8 MiB, size known
    let comp = std::fs::read(format!("{FX}/input.lzma1.raw")).unwrap();
    run_raw("LZMA1", "lzma-rust2 0.25.0", &comp, &src, |c| {
        Box::new(lzma_rust2::LzmaReader::new(c, LEN, 3, 0, 2, DICT, None).unwrap_or_else(|e| panic!("{e:?}")))
    }, &mut rows);

    // 3. LZMA2 raw stream (7z coder 21), dict 8 MiB
    let comp = std::fs::read(format!("{FX}/input.lzma2.raw")).unwrap();
    run_raw("LZMA2", "lzma-rust2 0.25.0", &comp, &src, |c| Box::new(lzma_rust2::Lzma2Reader::new(c, DICT, None)), &mut rows);

    // 4. RAR (rars fixtures). Reference = full decode of the untruncated file.
    for (name, path) in [
        ("RAR3 PPMd farmanager170.rar", "farmanager170.rar"),
        ("RAR5 m3_default.rar", "m3_default.rar"),
        ("RAR1.54 doc_154_best.rar", "doc_154_best.rar"),
        ("RAR3 unpack20_multiblock.rar", "unpack20_multiblock.rar"),
    ] {
        let full = std::fs::read(format!("{FX}/{path}")).unwrap();
        let (reference, ref_err) = rar_decode(full.clone());
        eprintln!("[rar] {name}: file={} B, full decode={} B ({ref_err})", full.len(), reference.len());
        for pct in PCTS {
            let take = full.len() * pct as usize / 100;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rar_decode(full[..take].to_vec())));
            let (out, err) = r.unwrap_or((Vec::new(), "PANIC".into()));
            let prefix_ok = out.len() <= reference.len() && reference[..out.len()] == out[..];
            rows.push(Row { codec: Box::leak(name.to_string().into_boxed_str()), crate_name: "rars 0.12.3", pct, take, out: out.len(), prefix_ok, err });
        }
    }

    println!("| codec | crate | truncation % | compressed bytes fed | output bytes | correct prefix? | terminal condition |");
    println!("|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("| {} | {} | {} | {} | {} | {} | {} |", r.codec, r.crate_name, r.pct, r.take, r.out, if r.prefix_ok { "yes" } else { "NO" }, r.err.replace('|', "/"));
    }
}

```
