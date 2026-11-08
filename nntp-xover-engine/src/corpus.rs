//! A deterministic corpus of binary-group overview lines for tests and
//! benchmarks, in the shapes `plan.md` asks for: multi-part yEnc posts with
//! `[A/B]`, `.partXX.` / `.XX.par2` and `(C/D)` counters in assorted
//! paddings, posts with several bracket groups, plain discussion posts,
//! message-ids split evenly between hex and Usenet base64 tails with 1% that
//! pack raw, and `Xref:` fields whose first number is the article number 95%
//! of the time, with one to three cross-posted groups.
//!
//! Every line has nine tab-separated fields and no line ending.

/// The seed [`generate_realistic_xover_corpus`] uses.
pub const DEFAULT_SEED: u64 = 0x853c_49e6_748f_ea9b;

/// Xorshift64*: deterministic, so every test and benchmark sees the same lines.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { DEFAULT_SEED } else { seed })
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
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
const B64: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-";
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
const HEX_DOMAINS: &[&str] = &["nyuu.local", "news.example.com", "ngPost.com"];
const B64_DOMAINS: &[&str] = &["blocknews.net", "powerpost2000AA.local", "JBinUp.local"];
const SECONDARY_GROUPS: &[&str] =
    &["alt.binaries.multimedia", "alt.binaries.boneless", "alt.binaries.misc", "alt.binaries.hdtv", "alt.binaries.tv"];
const DISCUSSION: &[&str] = &[
    "Re: Discussion without numbers",
    "Anyone have the complete series?",
    "Re: Re: Request - missing pars",
    "Thanks to the poster!",
    "Fill request",
    "Re: Fill request",
    "What happened to the usual poster",
    "OT: new reader recommendations?",
    "Re: OT: new reader recommendations?",
    "Repost please",
    "Re: Repost please",
    "Password?",
    "Re: Password?",
    "Reposted in a.b.multimedia",
    "Re: Thanks to the poster!",
    "FAQ: how to use par2",
];
const DISCUSSION_AUTHORS: &[&str] = &["reader <reader@example.net>", "lurker@invalid (Lurker)", "Jo <jo@home.example>"];

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
    /// 0 hex tails, 1 base64 tails
    msgid_style: u8,
    domain: &'static str,
    /// indices into SECONDARY_GROUPS
    xgroups: Vec<usize>,
    part_bytes: i64,
    channel_id: u32,
}

fn new_post(r: &mut Rng) -> Post {
    let name = format!("{}.{}.{}.{}", r.pick(WORDS), r.pick(WORDS), 1990 + r.below(36), r.pick(TAGS));
    let msgid_style = r.below(2) as u8;
    let mut xgroups: Vec<usize> = (0..SECONDARY_GROUPS.len()).collect();
    for i in (1..xgroups.len()).rev() {
        let j = r.below(i as u64 + 1) as usize;
        xgroups.swap(i, j);
    }
    xgroups.truncate(1 + r.below(3) as usize);
    Post {
        name,
        files: 3 + r.below(40) as u32,
        parts: 20 + r.below(100) as u32,
        file: 1,
        part: 1,
        shape: r.below(5) as u8,
        pads: (*r.pick(&[0usize, 2, 3]), *r.pick(&[0usize, 2, 3])),
        author: r.pick(AUTHORS),
        date_style: r.below(10) as u8,
        msgid_style,
        domain: if msgid_style == 0 { r.pick(HEX_DOMAINS) } else { r.pick(B64_DOMAINS) },
        xgroups,
        part_bytes: 700_000 + r.below(100_000) as i64,
        channel_id: 300_000 + r.below(100_000) as u32,
    }
}

fn pad(v: u32, w: usize) -> String {
    format!("{v:0w$}")
}

fn date(ts: i64, style: u8) -> String {
    let (off, zone) = match style {
        0..=5 => (0, None),
        6 => (-420, Some("(UTC)")),
        7 => (0, Some("GMT")),
        8 => (330, None),
        _ => (-300, None),
    };
    let mut buf = [0u8; crate::date::DATE_BUF];
    // the style word for these shapes: no weekday and an unpadded day for the GMT ones, a trailing ` (UTC)`
    // comment after -0700 for another posting tool
    let mut s = 0u32;
    match zone {
        Some("GMT") => s |= 1 | 2 | (1 << 2),
        Some(_) => s |= crate::date::COMMENT_UTC,
        None => {}
    }
    s |= (crate::varint::zigzag_encode(off) as u32) << 9;
    let n = crate::date::format_date(ts, s, &mut buf);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// The corpus the tests and `nxv-bench` use: `count` lines from [`DEFAULT_SEED`].
pub fn generate_realistic_xover_corpus(count: usize) -> Vec<Vec<u8>> {
    generate(count, DEFAULT_SEED)
}

/// `n` overview lines, ascending article numbers, deterministic in `seed`.
pub fn generate(n: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut r = Rng::new(seed);
    let mut art: u64 = 84_500_000;
    let mut secondary: Vec<u64> = (0..SECONDARY_GROUPS.len() as u64).map(|i| 19_200_400 + i * 7_654_321).collect();
    let mut ts: i64 = 1_762_500_000;
    let mut active: Vec<Post> = (0..3).map(|_| new_post(&mut r)).collect();
    let mut last_msgid = String::new();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        art += if r.chance(3) { 2 + r.below(5) } else { 1 };
        ts += r.below(3) as i64;
        let slot0 = if r.chance(95) { art } else { art + 7_000_000 + r.below(1000) };
        let mut xref = format!("Xref: news.example.com alt.binaries.teevee:{slot0}");

        // one row in a hundred is a discussion post: no counters, a tiny body, maybe a reply
        if r.chance(1) {
            let subject = *r.pick(DISCUSSION);
            let msgid = format!("<{}@news.individual.net>", r.chars(HEX, 32));
            let refs =
                if subject.starts_with("Re:") && !last_msgid.is_empty() { last_msgid.clone() } else { String::new() };
            let bytes = 300 + r.below(3000) as i64;
            let line = format!(
                "{art}\t{subject}\t{}\t{}\t{msgid}\t{refs}\t{bytes}\t{}\t{xref}",
                r.pick(DISCUSSION_AUTHORS),
                date(ts, 0),
                bytes / 40 + 1
            );
            last_msgid = msgid;
            out.push(line.into_bytes());
            continue;
        }

        // mostly the same poster as before, sometimes another one at the same time
        let pi = if r.chance(85) { 0 } else { r.below(active.len() as u64) as usize };
        let p = &mut active[pi];
        let (pw, fw) = p.pads;
        let subject = match p.shape {
            0 => format!(
                "[{}/{}] - \"{}.part{}.rar\" yEnc ({}/{})",
                pad(p.file, fw),
                pad(p.files, fw),
                p.name,
                pad(p.file, 3),
                pad(p.part, pw),
                p.parts
            ),
            1 => format!(
                "{} [{}/{}] - \"{}.vol{}+{}.par2\" yEnc ({}/{})",
                p.name,
                p.file,
                p.files,
                p.name,
                pad(p.file, 2),
                pad(p.file + 1, 2),
                pad(p.part, pw),
                pad(p.parts, pw)
            ),
            2 => format!("\"{}.{}.rar\" yEnc ({}/{})", p.name, pad(p.file, 2), pad(p.part, pw), p.parts),
            3 => format!(
                "[{}]-[FULL]-[#a.b.teevee@EFNet]-[ {} ]-[{}/{}] - \"{}.{}.par2\" yEnc ({}/{})",
                p.channel_id,
                p.name,
                pad(p.file, fw),
                pad(p.files, fw),
                p.name.to_ascii_lowercase(),
                pad(p.file, 2),
                pad(p.part, pw),
                p.parts
            ),
            _ => format!(
                "{} - [{}/{}] - \"{}.7z.{}\" ({}/{})",
                p.name,
                pad(p.file, fw),
                p.files,
                p.name,
                pad(p.file, 3),
                p.part,
                p.parts
            ),
        };
        let msgid = if r.chance(1) {
            format!("<weird+id_{}${}={}@custom.org>", r.below(1000), r.below(1000), r.below(1000))
        } else if p.msgid_style == 0 {
            format!("<part{}.{}.{}@{}>", pad(p.part, 2), r.chars(HEX, 16), r.chars(HEX, 8), p.domain)
        } else {
            format!("<seg{}.{}@{}>", pad(p.file, 2), r.chars(B64, 22), p.domain)
        };
        let refs = if r.chance(3) { format!("<{}@{}>", r.chars(B64, 12), p.domain) } else { String::new() };
        let bytes = if p.part == p.parts { p.part_bytes / 3 + r.below(1000) as i64 } else { p.part_bytes };
        let lines = bytes / 128 + 2;
        for &g in &p.xgroups {
            secondary[g] += 1 + r.below(2);
            xref.push_str(&format!(" {}:{}", SECONDARY_GROUPS[g], secondary[g]));
        }
        let line = format!(
            "{art}\t{subject}\t{}\t{}\t{msgid}\t{refs}\t{bytes}\t{lines}\t{xref}",
            p.author,
            date(ts, p.date_style)
        );
        last_msgid = msgid;
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
