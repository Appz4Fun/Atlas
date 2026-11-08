//! A deterministic corpus of binary-group overview lines for tests and
//! benchmarks: multi-part yEnc posts with assorted counter paddings, several
//! posters at once, cross-posted `Xref:`, a few `References`, and message-ids
//! in the shapes posting tools make (hex, base64-ish, prefixed, odd bytes).

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
    fn chars(&mut self, alphabet: &[u8], n: usize) -> String {
        (0..n).map(|_| *self.pick(alphabet) as char).collect()
    }
}

const HEX: &[u8] = b"0123456789abcdef";
const B64: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const WORDS: &[&str] = &[
    "The", "Last", "Night", "Ocean", "Dark", "City", "Lost", "Signal", "Winter", "Garden", "Empire", "Shadow", "River",
    "Station", "Echo", "Frontier",
];
const TAGS: &[&str] = &[
    "1080p.WEB-DL.DDP5.1.H.264-NTb",
    "2160p.UHD.BluRay.x265-TERMiNAL",
    "720p.HDTV.x264-SYNCOPY",
    "1080p.BluRay.x264-AMIABLE",
    "S01E04.1080p.WEB.h264-ETHEL",
];
const AUTHORS: &[&str] = &[
    "Poster <poster@example.com>",
    "yEncBin@Poster.com (yEncBin)",
    "JBinUp.local <JBinUp.local@JBinUp.local>",
    "Nyuu <nyuu@nyuu.invalid>",
    "anon <anon@anon.invalid>",
    "PowerPost <pp@power.post>",
    "Ninja <ninja@ng.net>",
    "Mr. Hyde <hyde@jekyll.example>",
];
const DOMAINS: &[&str] =
    &["news.example.com", "powerpost2000AA.local", "nyuu", "JBinUp.local", "ngPost.com", "xyz.invalid"];

struct Post {
    name: String,
    files: u32,
    parts: u32,
    file: u32,
    part: u32,
    shape: u8,
    pads: (usize, usize),
    author: &'static str,
    date_style: u8,
    msgid_style: u8,
    domain: &'static str,
    part_bytes: i64,
}

fn new_post(r: &mut Rng) -> Post {
    let name = format!("{}.{}.{}.{}", r.pick(WORDS), r.pick(WORDS), 1990 + r.below(36), r.pick(TAGS));
    Post {
        name,
        files: 3 + r.below(40) as u32,
        parts: 10 + r.below(110) as u32,
        file: 1,
        part: 1,
        shape: r.below(4) as u8,
        pads: (*r.pick(&[0usize, 2, 3]), *r.pick(&[0usize, 2, 3])),
        author: r.pick(AUTHORS),
        date_style: r.below(10) as u8,
        msgid_style: r.below(4) as u8,
        domain: r.pick(DOMAINS),
        part_bytes: 700_000 + r.below(100_000) as i64,
    }
}

fn pad(v: u32, w: usize) -> String {
    format!("{v:0w$}")
}

fn date(ts: i64, style: u8) -> String {
    let (off, zone) = match style {
        0..=6 => (0, None),
        7 => (0, Some("GMT")),
        8 => (330, None),
        _ => (-300, None),
    };
    let mut buf = [0u8; crate::date::DATE_BUF];
    // build the style word for these shapes
    let mut s = 0u32;
    if zone.is_some() {
        s |= 1 | 2 | (1 << 2);
    }
    s |= (crate::varint::zigzag_encode(off) as u32) << 8;
    let n = crate::date::format_date(ts, s, &mut buf);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// `n` overview lines, ascending article numbers, deterministic in `seed`.
pub fn generate(n: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut r = Rng::new(seed);
    let mut art: u64 = 84_500_000;
    let mut other: u64 = 19_200_400;
    let mut ts: i64 = 1_762_500_000;
    let mut active: Vec<Post> = (0..3).map(|_| new_post(&mut r)).collect();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        art += if r.chance(3) { 2 + r.below(5) } else { 1 };
        ts += r.below(3) as i64;
        // mostly the same poster as before, sometimes another one at the same time
        let pi = if r.chance(85) { 0 } else { r.below(active.len() as u64) as usize };
        let p = &mut active[pi];
        let (pw, fw) = p.pads;
        let filename = match p.shape {
            0 => format!("{}.part{}.rar", p.name, pad(p.file, 3)),
            1 => format!("{}.vol{}+{}.par2", p.name, pad(p.file, 2), pad(p.file + 1, 2)),
            2 => format!("{}.{}.rar", p.name, pad(p.file, 2)),
            _ => format!("{}.7z.{}", p.name, pad(p.file, 3)),
        };
        let subject = match p.shape {
            0 => format!(
                "[{}/{}] - \"{}\" yEnc ({}/{})",
                pad(p.file, fw),
                pad(p.files, fw),
                filename,
                pad(p.part, pw),
                p.parts
            ),
            1 => format!(
                "{} [{}/{}] - \"{}\" yEnc ({}/{})",
                p.name,
                p.file,
                p.files,
                filename,
                pad(p.part, pw),
                pad(p.parts, pw)
            ),
            2 => format!("\"{}\" yEnc ({}/{})", filename, pad(p.part, pw), p.parts),
            _ => format!("{} - [{}/{}] - \"{}\" ({}/{})", p.name, pad(p.file, fw), p.files, filename, p.part, p.parts),
        };
        let msgid = match p.msgid_style {
            0 => format!("<{}.{}@{}>", r.chars(HEX, 16), r.chars(HEX, 8), p.domain),
            1 => format!("<{}@{}>", r.chars(B64, 22), p.domain),
            2 => format!("<part{}of{}.{}@{}>", p.part, p.parts, r.chars(B64, 20), p.domain),
            _ => format!("<{}${}@{}>", r.chars(HEX, 12), p.part, p.domain),
        };
        let refs = if r.chance(3) { format!("<{}@{}>", r.chars(B64, 12), p.domain) } else { String::new() };
        let bytes = if p.part == p.parts { p.part_bytes / 3 + r.below(1000) as i64 } else { p.part_bytes };
        let lines = bytes / 128 + 2;
        let mut xref = format!("Xref: news.example.com alt.binaries.teevee:{art}");
        if r.chance(30) {
            other += 1;
            xref.push_str(&format!(" alt.binaries.multimedia:{other}"));
        }
        let line = format!(
            "{art}\t{subject}\t{}\t{}\t{msgid}\t{refs}\t{bytes}\t{lines}\t{xref}",
            p.author,
            date(ts, p.date_style)
        );
        out.push(line.into_bytes());
        p.part += 1;
        if p.part > p.parts {
            p.part = 1;
            p.file += 1;
            if p.file > p.files {
                active[pi] = new_post(&mut r);
            }
        }
    }
    out
}
