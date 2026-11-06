//! How an encoder wrote its yEnc, read from the raw lines of the articles the probe fetches: the order of the
//! keys in `=ybegin`, `=ypart` and `=yend`, how it prints a CRC, and which of the characters yEnc leaves to the
//! encoder it did escape. Posting tools differ in all of these (the research's tool table,
//! `docs/research/yenc-header-forensics/opus/w8c-fingerprint-table.md`, and `w1a-encoder-quirks.md`), soo they name
//! the encoder even when the Message-ID and User-Agent are faked or left to the server.
//!
//! It names a tool family, never an uploader: thousands of posts share one, soo it's a witness for the posting-tool
//! verdict and a stored fact, not a key.

#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use serde::{Deserialize, Serialize};

/// Two counts: how often a thing was done and how often it wasn't (an escape and a raw byte, capitals and small
/// letters, a padded CRC and an unpadded one).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Votes(pub u32, pub u32);

impl Votes {
    fn yes(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
    fn no(&mut self) {
        self.1 = self.1.saturating_add(1);
    }
    fn vote(&mut self, yes: bool) {
        if yes { self.yes() } else { self.no() }
    }
    fn add(&mut self, other: Votes) {
        self.0 = self.0.saturating_add(other.0);
        self.1 = self.1.saturating_add(other.1);
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0 && self.1 == 0
    }
    /// What the votes say: always, never, or None when there were none or both.
    pub fn says(&self) -> Option<bool> {
        match (self.0 > 0, self.1 > 0) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }
}

/// The characters an encoder may escape or leave, by where in a line they fall. A vote is (escaped, raw).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escapes {
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub lead_tab: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub lead_space: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub lead_dot: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub end_tab: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub end_space: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub mid_dot: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub mid_tab: Votes,
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub mid_space: Votes,
    /// 0x7F, which nxg-upper escapes
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub del: Votes,
}

impl Escapes {
    fn add(&mut self, o: &Escapes) {
        for (mine, theirs) in self.all_mut().into_iter().zip(o.all()) {
            mine.add(theirs);
        }
    }
    fn all(&self) -> [Votes; 9] {
        [
            self.lead_tab,
            self.lead_space,
            self.lead_dot,
            self.end_tab,
            self.end_space,
            self.mid_dot,
            self.mid_tab,
            self.mid_space,
            self.del,
        ]
    }
    fn all_mut(&mut self) -> [&mut Votes; 9] {
        [
            &mut self.lead_tab,
            &mut self.lead_space,
            &mut self.lead_dot,
            &mut self.end_tab,
            &mut self.end_space,
            &mut self.mid_dot,
            &mut self.mid_tab,
            &mut self.mid_space,
            &mut self.del,
        ]
    }
    fn is_empty(&self) -> bool {
        self.all().iter().all(Votes::is_empty)
    }
}

/// What the names of `Escapes`' fields say, in its order.
const ESCAPE_NAMES: [&str; 9] = [
    "leading tabs",
    "leading spaces",
    "leading dots",
    "trailing tabs",
    "trailing spaces",
    "inner dots",
    "inner tabs",
    "inner spaces",
    "DEL bytes",
];

/// An encoder's habits, from the first article the probe reads and the last, when it reads one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quirks {
    /// the keys of `=ybegin` in the order written, `name` last as it runs to the end: `part total line size name`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub begin: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ypart: String,
    /// the keys of the `=yend` of a part that isn't the file's last
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub yend: String,
    /// the keys of the last part's `=yend`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_yend: String,
    /// CRC hex in capitals, or in small letters
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub upper: Votes,
    /// a CRC zero-padded to 8 digits, or one with its leading zeros dropped. A CRC of 8 digits that starts with
    /// another digit says nothing.
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub padded: Votes,
    /// `name=` ends in a space, as the draft's example does (yEnc32)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_space: bool,
    /// an empty line before `=yend`, or, in a part of whole lines, none
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub blank_before_yend: Votes,
    /// an empty line after `=yend`
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub blank_after_yend: Votes,
    /// a data line longer than `line=` and one escape (PowerPost counts its 128 before encoding)
    #[serde(default, skip_serializing_if = "Votes::is_empty")]
    pub over_long: Votes,
    #[serde(default, skip_serializing_if = "Escapes::is_empty")]
    pub escapes: Escapes,
}

/// A key the control lines may hold: a short word.
fn is_key(k: &str) -> bool {
    (1..=8).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The keys of a control line in order, after its `=y...` word, stopping at `name` or anything that isn't a key.
fn keys(line: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for token in line.split(' ').skip(1).filter(|t| !t.is_empty()) {
        let Some((key, _)) = token.split_once('=') else { break };
        if !is_key(key) || out.len() >= 12 {
            break;
        }
        out.push(key);
        if key == "name" {
            break;
        }
    }
    out.join(" ")
}

/// The value of `key=` in a control line, as written.
fn raw_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ').skip(1).find_map(|t| t.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

fn has_key(keys: &str, key: &str) -> bool {
    keys.split(' ').any(|k| k == key)
}

impl Quirks {
    /// Reads an article's raw body lines, as `yenc::split_article` gives them. Whether it's the file's last part
    /// comes from the article alone: `part=N total=N`, or a `=ypart end=` that is the `=ybegin size=`. An article
    /// that is merely the last one indexed proves nothing, soo its `=yend` counts as a middle part's. None: no
    /// `=ybegin`.
    pub fn read(lines: &[Vec<u8>]) -> Option<Quirks> {
        let start = lines.iter().position(|l| l.starts_with(b"=ybegin "))?;
        let mut q = Quirks::default();
        let begin = String::from_utf8_lossy(lines.get(start)?);
        q.begin = keys(&begin);
        q.name_space = begin.contains(" name=") && begin.ends_with(' ');
        let number = |k: &str| raw_value(&begin, k).and_then(|v| v.trim().parse::<u64>().ok());
        let line_len = number("line").unwrap_or(128);
        let rest = lines.get(start + 1..).unwrap_or_default();
        let ypart = rest.first().filter(|l| l.starts_with(b"=ypart ")).map(|l| String::from_utf8_lossy(l));
        let ypart_end = ypart.as_deref().and_then(|l| raw_value(l, "end")).and_then(|v| v.trim().parse::<u64>().ok());
        let is_last = (number("part").is_some() && number("part") == number("total"))
            || (ypart_end.is_some() && ypart_end == number("size"));
        let mut longest_ok = true;
        let mut previous_empty = false;
        let mut data_lines: Vec<&[u8]> = Vec::new();
        let mut yend_at = None;
        for (at, line) in rest.iter().enumerate() {
            if at == 0 && line.starts_with(b"=ypart ") {
                q.ypart = keys(&String::from_utf8_lossy(line));
                continue;
            }
            if line.starts_with(b"=yend") {
                yend_at = Some(at);
                break;
            }
            previous_empty = line.is_empty();
            data_lines.push(line);
        }
        let count = data_lines.len();
        for (n, line) in data_lines.into_iter().enumerate() {
            // the last data line is short, or ends the part; an over-long one is a habit only before it
            if n + 1 < count && line.len() as u64 > line_len.saturating_add(1) {
                longest_ok = false;
            }
            q.escapes_of(line);
        }
        if count > 1 {
            q.over_long.vote(!longest_ok);
        }
        if let Some(at) = yend_at {
            let text = String::from_utf8_lossy(rest.get(at).map(Vec::as_slice).unwrap_or_default());
            let end = keys(&text);
            for key in ["pcrc32", "crc32"] {
                if let Some(v) = raw_value(&text, key) {
                    q.crc_style(v.trim());
                }
            }
            let size = raw_value(&text, "size").and_then(|v| v.trim().parse::<u64>().ok());
            let whole = size.is_some_and(|s| s > 0 && line_len > 0 && s % line_len == 0);
            if previous_empty {
                q.blank_before_yend.yes();
            } else if whole {
                q.blank_before_yend.no();
            }
            let after = rest.get(at + 1..).unwrap_or_default();
            q.blank_after_yend.vote(after.first().is_some_and(|l| l.is_empty()));
            if is_last {
                q.last_yend = end;
            } else {
                q.yend = end;
            }
        }
        Some(q)
    }

    /// The case and padding a CRC is written in.
    fn crc_style(&mut self, v: &str) {
        if v.is_empty() || v.len() > 8 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
            return;
        }
        if v.bytes().any(|b| b.is_ascii_uppercase()) {
            self.upper.yes();
        }
        if v.bytes().any(|b| b.is_ascii_lowercase()) {
            self.upper.no();
        }
        if v.len() < 8 {
            self.padded.no();
        } else if v.starts_with('0') {
            self.padded.yes();
        }
    }

    /// Counts the optional characters in one data line, escaped and raw, by where they fall.
    fn escapes_of(&mut self, line: &[u8]) {
        let len = line.len();
        let mut i = 0;
        while i < len {
            let Some(&b) = line.get(i) else { break };
            let (byte, escaped, width) = if b == b'=' {
                match line.get(i + 1) {
                    Some(&next) => (next.wrapping_sub(64), true, 2),
                    // a lone `=` at the end is broken, not a habit
                    None => break,
                }
            } else {
                (b, false, 1)
            };
            let lead = i == 0;
            let end = i + width == len;
            let e = &mut self.escapes;
            let vote = match byte {
                b'\t' if lead => Some(&mut e.lead_tab),
                b'\t' if end => Some(&mut e.end_tab),
                b'\t' => Some(&mut e.mid_tab),
                b' ' if lead => Some(&mut e.lead_space),
                b' ' if end => Some(&mut e.end_space),
                b' ' => Some(&mut e.mid_space),
                b'.' if lead => Some(&mut e.lead_dot),
                b'.' => Some(&mut e.mid_dot),
                0x7f => Some(&mut e.del),
                _ => None,
            };
            if let Some(v) = vote {
                v.vote(escaped);
            }
            i += width;
        }
    }

    /// Folds in what another article of the same file showed.
    pub fn add(&mut self, o: &Quirks) {
        for (mine, theirs) in [
            (&mut self.begin, &o.begin),
            (&mut self.ypart, &o.ypart),
            (&mut self.yend, &o.yend),
            (&mut self.last_yend, &o.last_yend),
        ] {
            if mine.is_empty() {
                mine.clone_from(theirs);
            }
        }
        self.upper.add(o.upper);
        self.padded.add(o.padded);
        self.name_space |= o.name_space;
        self.blank_before_yend.add(o.blank_before_yend);
        self.blank_after_yend.add(o.blank_after_yend);
        self.over_long.add(o.over_long);
        self.escapes.add(&o.escapes);
    }

    /// A one-file part (`=ybegin` without `part=`): its `=yend` is the single-part form, which tells little.
    fn single(&self) -> bool {
        !self.begin.is_empty() && !has_key(&self.begin, "part")
    }

    /// The fingerprint in a line: `=ybegin part total line size name; =ypart begin end; =yend size part pcrc32,
    /// crc32 on the last; lowercase zero-padded CRCs; escapes leading tabs, ...`.
    pub fn summary(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        if !self.begin.is_empty() {
            out.push(format!("=ybegin {}", self.begin));
        }
        if !self.ypart.is_empty() {
            out.push(format!("=ypart {}", self.ypart));
        }
        let crc32_free = |k: &str| k.split(' ').filter(|k| *k != "crc32").collect::<Vec<_>>().join(" ");
        match (self.yend.is_empty(), self.last_yend.is_empty()) {
            (false, true) => out.push(format!("=yend {}", self.yend)),
            (true, false) => out.push(format!("last =yend {}", self.last_yend)),
            (false, false) if crc32_free(&self.last_yend) == self.yend => {
                let on_last =
                    if has_key(&self.last_yend, "crc32") { "crc32 on the last" } else { "no crc32 on the last" };
                out.push(format!("=yend {}, {on_last}", self.yend));
            }
            (false, false) => out.push(format!("=yend {}, last =yend {}", self.yend, self.last_yend)),
            (true, true) => {}
        }
        let case = match self.upper.says() {
            Some(true) => Some("capital"),
            Some(false) => Some("lowercase"),
            None => None,
        };
        let pad = match self.padded.says() {
            Some(true) => Some("zero-padded"),
            Some(false) => Some("unpadded"),
            None => None,
        };
        let style: Vec<&str> = [case, pad].into_iter().flatten().collect();
        if !style.is_empty() {
            out.push(format!("{} CRCs", style.join(" ")));
        }
        let all = self.escapes.all();
        let named = |want: bool| -> Vec<&str> {
            all.iter().zip(ESCAPE_NAMES).filter(|(v, _)| v.says() == Some(want)).map(|(_, n)| n).collect()
        };
        let (escaped, raw) = (named(true), named(false));
        if !escaped.is_empty() {
            out.push(format!("escapes {}", escaped.join(", ")));
        }
        // the edges and dots are what tell encoders apart: a raw inner tab is what nearly all of them do
        let raw: Vec<&str> = raw.into_iter().filter(|n| !n.starts_with("inner") && *n != "DEL bytes").collect();
        if !raw.is_empty() {
            out.push(format!("leaves {} raw", raw.join(", ")));
        }
        if self.blank_before_yend.says() == Some(true) {
            out.push("an empty line before =yend".into());
        }
        if self.blank_after_yend.says() == Some(true) {
            out.push("an empty line after =yend".into());
        }
        if self.name_space {
            out.push("a space after the name".into());
        }
        if self.over_long.says() == Some(true) {
            out.push("lines longer than line=".into());
        }
        out.join("; ")
    }
}

/// What one tool's yEnc looks like, as the research read it in the tool's source. None: not known, or it varies.
struct Profile {
    name: &'static str,
    /// what its User-Agent or X-Newsposter holds, lowercased
    agents: &'static [&'static str],
    begin: Option<&'static str>,
    /// whether `=ybegin` has `total=`, for a tool whose key order isn't known
    total: Option<bool>,
    ypart: Option<&'static str>,
    /// the `=yend` keys of a part before the last
    yend: Option<&'static str>,
    crc32_last: Option<bool>,
    upper: Option<bool>,
    padded: Option<bool>,
    name_space: Option<bool>,
    blank_before: Option<bool>,
    blank_after: Option<bool>,
    over_long: Option<bool>,
    /// in `Escapes`' order: escaped (true) or left raw (false)
    escapes: [Option<bool>; 9],
}

const T: Option<bool> = Some(true);
const F: Option<bool> = Some(false);
const N: Option<bool> = None;

/// RapidYenc / node-yencode (Nyuu, postie): tab and space at both edges and a leading dot, nothing inside.
const RAPIDYENC: [Option<bool>; 9] = [T, T, T, T, T, F, F, F, F];

const NONE: Profile = Profile {
    name: "",
    agents: &[],
    begin: None,
    total: None,
    ypart: None,
    yend: None,
    crc32_last: None,
    upper: None,
    padded: None,
    name_space: None,
    blank_before: None,
    blank_after: None,
    // a 128-byte line counts encoded bytes, soo only PowerPost's run past one escape
    over_long: F,
    escapes: [N; 9],
};

/// The tools whose yEnc the research documents (w8c's table, rows as cited). JBinUp's yEnc isn't recorded, soo
/// its claim gets no witness here.
const PROFILES: &[Profile] = &[
    // row 1: always multipart, crc32 on the last part only
    Profile {
        name: "Nyuu",
        agents: &["nyuu"],
        begin: Some("part total line size name"),
        ypart: Some("begin end"),
        yend: Some("size part pcrc32"),
        crc32_last: T,
        upper: F,
        padded: T,
        name_space: F,
        blank_before: F,
        escapes: RAPIDYENC,
        ..NONE
    },
    // row 5: rapidyenc too, but no crc32 on the last part
    Profile {
        name: "postie",
        agents: &["postie"],
        ypart: Some("begin end"),
        yend: Some("size part pcrc32"),
        crc32_last: F,
        upper: F,
        padded: T,
        name_space: F,
        escapes: RAPIDYENC,
        ..NONE
    },
    // row 8: no part= or crc32= in =yend, std::hex unpadded, the end-of-line test never fires
    Profile {
        name: "ngPost",
        agents: &["ngpost"],
        yend: Some("size pcrc32"),
        crc32_last: F,
        upper: F,
        padded: F,
        name_space: F,
        blank_before: T,
        escapes: [N, N, T, F, F, F, N, N, N],
        ..NONE
    },
    // row 10: the fork pads its CRCs and escapes trailing whitespace
    Profile {
        name: "ngPost fork (5.6+)",
        agents: &["ngpost"],
        yend: Some("size pcrc32"),
        crc32_last: F,
        upper: F,
        padded: T,
        name_space: F,
        blank_before: T,
        escapes: [N, N, T, T, T, F, N, N, N],
        ..NONE
    },
    // row 12
    Profile {
        name: "NewsUP",
        agents: &["newsup"],
        begin: Some("part total line size name"),
        yend: Some("size pcrc32"),
        crc32_last: F,
        upper: F,
        padded: F,
        name_space: F,
        blank_before: T,
        escapes: [N, N, N, F, F, F, N, N, N],
        ..NONE
    },
    // row 14: %08X, escapes the first and last column, no dot-stuffing soo a leading dot is escaped
    Profile {
        name: "GoPostStuff",
        agents: &["gopoststuff"],
        yend: Some("size part pcrc32"),
        crc32_last: F,
        upper: T,
        padded: T,
        name_space: F,
        escapes: [T, T, T, T, T, F, N, N, N],
        ..NONE
    },
    // row 15: python-yenc escapes whitespace at both ends and dot-stuffs a leading dot
    Profile {
        name: "newsmangler",
        agents: &["newsmangler"],
        yend: Some("size part pcrc32"),
        crc32_last: F,
        upper: F,
        padded: T,
        name_space: F,
        escapes: [T, T, F, T, T, F, N, N, N],
        ..NONE
    },
    // row 7: size= in =ypart, capitals, escapes DEL and edge spaces but not tabs
    Profile {
        name: "nxg-upper",
        agents: &["nxg"],
        ypart: Some("begin end size"),
        yend: Some("size part pcrc32"),
        crc32_last: T,
        upper: T,
        padded: T,
        name_space: F,
        escapes: [F, T, N, F, T, F, N, N, T],
        ..NONE
    },
    // row 16: no total=, escapes only the four critical bytes, 128 input bytes a line, a blank line at the end.
    // Whether its =yend has pcrc32 is unsettled (conflict X7).
    Profile {
        name: "PowerPost",
        agents: &["powerpost"],
        begin: Some("part line size name"),
        upper: F,
        padded: T,
        name_space: F,
        blank_after: T,
        over_long: N,
        escapes: [F, F, F, F, F, F, F, F, F],
        ..NONE
    },
    // row 21: the draft's trailing space after the name, every dot escaped, never a space
    Profile {
        name: "yEnc32",
        agents: &["yenc32", "tyencoder"],
        yend: Some("size part pcrc32"),
        crc32_last: T,
        padded: T,
        name_space: T,
        escapes: [N, F, T, N, F, T, N, F, N],
        ..NONE
    },
    // row 22: escapes every tab and a leading dot, never a space
    Profile {
        name: "newspost",
        agents: &["newspost"],
        yend: Some("size part pcrc32"),
        crc32_last: T,
        upper: F,
        padded: T,
        name_space: F,
        escapes: [T, F, T, T, F, F, T, F, F],
        ..NONE
    },
    // row 26 and w1a: the Rust yenc crate, no total=, never escapes whitespace, dot-stuffs, a blank line on a
    // part of whole lines
    Profile {
        name: "Rust yenc crate (pesto)",
        agents: &["pesto"],
        total: F,
        upper: F,
        padded: T,
        name_space: F,
        blank_before: T,
        escapes: [F, F, F, F, F, F, N, N, N],
        ..NONE
    },
];

/// How a profile fares against what was seen: the habits it shares, and the first ones it can't have.
struct Fit {
    agree: usize,
    against: Vec<String>,
}

fn fit(p: &Profile, q: &Quirks) -> Fit {
    let mut f = Fit { agree: 0, against: Vec::new() };
    let mut check = |seen: Option<bool>, want: Option<bool>, yes: &str, no: &str| {
        if let (Some(seen), Some(want)) = (seen, want) {
            if seen == want {
                f.agree += 1;
            } else {
                f.against.push(if seen { yes } else { no }.to_string());
            }
        }
    };
    let words = |seen: &str, want: Option<&str>| (!seen.is_empty() && want.is_some()).then(|| Some(seen) == want);
    check(words(&q.begin, p.begin), p.begin.map(|_| true), "", &format!("=ybegin {}", q.begin));
    if !q.begin.is_empty() {
        check(Some(has_key(&q.begin, "total")), p.total, "total= in =ybegin", "no total= in =ybegin");
    }
    check(words(&q.ypart, p.ypart), p.ypart.map(|_| true), "", &format!("=ypart {}", q.ypart));
    if !q.single() {
        check(words(&q.yend, p.yend), p.yend.map(|_| true), "", &format!("=yend {}", q.yend));
        if !q.last_yend.is_empty() {
            let base: Vec<&str> = q.last_yend.split(' ').filter(|k| *k != "crc32").collect();
            let base = base.join(" ");
            check(words(&base, p.yend), p.yend.map(|_| true), "", &format!("=yend {}", q.last_yend));
            check(
                Some(has_key(&q.last_yend, "crc32")),
                p.crc32_last,
                "crc32 on the last part",
                "no crc32 on the last part",
            );
        }
    }
    check(q.upper.says(), p.upper, "capital CRCs", "lowercase CRCs");
    check(q.padded.says(), p.padded, "zero-padded CRCs", "unpadded CRCs");
    check(
        Some(q.name_space),
        p.name_space.filter(|_| !q.begin.is_empty()),
        "a space after the name",
        "no space after the name",
    );
    check(q.blank_before_yend.says(), p.blank_before, "an empty line before =yend", "no empty line before =yend");
    check(q.blank_after_yend.says(), p.blank_after, "an empty line after =yend", "no empty line after =yend");
    check(q.over_long.says(), p.over_long, "lines longer than line=", "lines no longer than line=");
    for ((seen, want), name) in q.escapes.all().iter().zip(p.escapes).zip(ESCAPE_NAMES) {
        check(seen.says(), want, &format!("escaped {name}"), &format!("raw {name}"));
    }
    f
}

/// Whether what was read holds anything to compare.
fn has_evidence(q: &Quirks) -> bool {
    !q.begin.is_empty() || !q.yend.is_empty() || !q.last_yend.is_empty() || !q.upper.is_empty() || !q.padded.is_empty()
}

/// The tools whose documented yEnc fits every habit seen, the best fit first. Empty: none of them.
pub fn lookalikes(q: &Quirks) -> Vec<&'static str> {
    if !has_evidence(q) {
        return Vec::new();
    }
    let mut fits: Vec<(usize, &'static str)> = PROFILES
        .iter()
        .filter_map(|p| {
            let f = fit(p, q);
            (f.against.is_empty() && f.agree > 0).then_some((f.agree, p.name))
        })
        .collect();
    fits.sort_by_key(|(agree, _)| std::cmp::Reverse(*agree));
    fits.into_iter().map(|(_, name)| name).collect()
}

/// The family, as the page shows it: `Nyuu or postie`, or None when nothing fits or nothing was read.
pub fn class(q: &Quirks) -> Option<String> {
    let found = lookalikes(q);
    (!found.is_empty()).then(|| found.into_iter().take(3).collect::<Vec<_>>().join(" or "))
}

/// The quirks as a witness for a stated tool: (agrees, why). None when the tool's yEnc isn't documented or
/// nothing was read.
pub fn witness(q: &Quirks, agent: &str) -> Option<(bool, String)> {
    let agent = agent.to_ascii_lowercase();
    let claimed: Vec<&Profile> = PROFILES.iter().filter(|p| p.agents.iter().any(|a| agent.contains(a))).collect();
    if claimed.is_empty() || !has_evidence(q) {
        return None;
    }
    let found = lookalikes(q);
    if let Some(p) = claimed.iter().find(|p| found.contains(&p.name)) {
        return Some((true, format!("{} yEnc quirks", p.name)));
    }
    Some((
        false,
        match class(q) {
            Some(class) => format!("yEnc quirks of {class}"),
            None => {
                let mut against = claimed.first().map(|p| fit(p, q).against).unwrap_or_default();
                against.truncate(2);
                format!("yEnc quirks ({})", against.join(", "))
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: usize = 128;

    /// A part's lines as one tool writes them: `escape` says whether an encoded byte at (line start, line end) is
    /// escaped. A leading dot left raw reads back raw, as the NNTP client undoes the dot-stuffing.
    fn encode(data: &[u8], escape: &dyn Fn(u8, bool, bool) -> bool) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        let mut line = Vec::new();
        let mut n = 0;
        while n < data.len() {
            let e = data[n].wrapping_add(42);
            let lead = line.is_empty();
            let end = line.len() + 1 >= LINE || n + 1 == data.len();
            let critical = matches!(e, 0 | b'\n' | b'\r' | b'=');
            if critical || escape(e, lead, end) {
                line.push(b'=');
                line.push(e.wrapping_add(64));
            } else {
                line.push(e);
            }
            n += 1;
            if line.len() >= LINE {
                lines.push(std::mem::take(&mut line));
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// Noise from an LCG: over 2,000 lines, every optional character falls at the start, the end and the middle of
    /// some line.
    fn data(len: usize) -> Vec<u8> {
        let mut x: u32 = 12345;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect()
    }

    fn article(begin: &str, ypart: Option<&str>, body: Vec<Vec<u8>>, blank: bool, yend: &str) -> Vec<Vec<u8>> {
        let mut out = vec![begin.as_bytes().to_vec()];
        if let Some(p) = ypart {
            out.push(p.as_bytes().to_vec());
        }
        out.extend(body);
        if blank {
            out.push(Vec::new());
        }
        out.push(yend.as_bytes().to_vec());
        out
    }

    const SIZE: usize = 128 * 2000;

    fn rapid(e: u8, lead: bool, end: bool) -> bool {
        (matches!(e, b'\t' | b' ') && (lead || end)) || (e == b'.' && lead)
    }

    fn nyuu() -> Quirks {
        let body = encode(&data(SIZE), &rapid);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body.clone(),
            false,
            "=yend size=5120 part=1 pcrc32=0badf00d",
        );
        let last = article(
            "=ybegin part=3 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=10241 end=15360"),
            body,
            false,
            "=yend size=5120 part=3 pcrc32=1234abcd crc32=00c0ffee",
        );
        let mut q = Quirks::read(&first).unwrap();
        q.add(&Quirks::read(&last).unwrap());
        q
    }

    #[test]
    fn nyuu_is_read_and_named() {
        let q = nyuu();
        assert_eq!(q.begin, "part total line size name");
        assert_eq!(q.ypart, "begin end");
        assert_eq!(q.yend, "size part pcrc32");
        assert_eq!(q.last_yend, "size part pcrc32 crc32");
        assert_eq!((q.upper.says(), q.padded.says()), (Some(false), Some(true)));
        for v in [q.escapes.lead_tab, q.escapes.lead_space, q.escapes.lead_dot, q.escapes.end_tab, q.escapes.end_space]
        {
            assert_eq!(v.says(), Some(true), "{q:?}");
        }
        assert_eq!(q.escapes.mid_dot.says(), Some(false));
        assert_eq!(q.blank_before_yend.says(), Some(false));
        assert_eq!(lookalikes(&q), ["Nyuu"]);
        assert_eq!(witness(&q, "Nyuu/0.4.2"), Some((true, "Nyuu yEnc quirks".into())));
        assert_eq!(witness(&q, "ngPost v4.16"), Some((false, "yEnc quirks of Nyuu".into())));
        assert_eq!(witness(&q, "Mozilla Thunderbird"), None);
        assert_eq!(
            q.summary(),
            "=ybegin part total line size name; =ypart begin end; =yend size part pcrc32, crc32 on the last; \
             lowercase zero-padded CRCs; escapes leading tabs, leading spaces, leading dots, trailing tabs, \
             trailing spaces"
        );
    }

    #[test]
    fn the_first_article_alone_cant_tell_nyuu_from_postie() {
        let body = encode(&data(SIZE), &rapid);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body.clone(),
            false,
            "=yend size=5120 part=1 pcrc32=0badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(class(&q).as_deref(), Some("Nyuu or postie"));
        // postie leaves crc32 off the last part
        let last = article(
            "=ybegin part=3 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=10241 end=15360"),
            body,
            false,
            "=yend size=5120 part=3 pcrc32=0234abcd",
        );
        let mut q = q;
        q.add(&Quirks::read(&last).unwrap());
        assert_eq!(lookalikes(&q), ["postie"]);
        assert_eq!(witness(&q, "Nyuu/0.4.2"), Some((false, "yEnc quirks of postie".into())));
    }

    #[test]
    fn ngpost_and_newsup_write_no_part_in_yend() {
        let ng = |e: u8, lead: bool, _end: bool| (e == b'.' || matches!(e, b'\t' | b' ')) && lead;
        let body = encode(&data(SIZE), &ng);
        // 5120 is 40 whole lines: the blank line before =yend
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            true,
            "=yend size=5120 pcrc32=badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.yend, "size pcrc32");
        assert_eq!(q.padded.says(), Some(false));
        assert_eq!(q.escapes.end_space.says(), Some(false));
        assert_eq!(q.blank_before_yend.says(), Some(true));
        assert_eq!(lookalikes(&q), ["ngPost", "NewsUP"]);
        assert_eq!(witness(&q, "ngPost v4.16").map(|w| w.0), Some(true));
        assert_eq!(witness(&q, "NewsUP"), Some((true, "NewsUP yEnc quirks".into())));
        assert_eq!(witness(&q, "Nyuu/0.4.2"), Some((false, "yEnc quirks of ngPost or NewsUP".into())));
    }

    #[test]
    fn the_ngpost_fork_pads_and_escapes_trailing_whitespace() {
        let body = encode(&data(SIZE), &rapid);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            true,
            "=yend size=5120 pcrc32=0badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(lookalikes(&q), ["ngPost fork (5.6+)"]);
        assert_eq!(witness(&q, "ngPost").map(|w| w.0), Some(true), "the fork is an ngPost");
    }

    #[test]
    fn gopoststuff_writes_capitals() {
        let body = encode(&data(SIZE), &rapid);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=0BADF00D",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.upper.says(), Some(true));
        assert_eq!(lookalikes(&q), ["GoPostStuff"]);
        assert_eq!(witness(&q, "gopoststuff 0.2.0").map(|w| w.0), Some(true));
    }

    #[test]
    fn newsmangler_leaves_a_leading_dot_to_dot_stuffing() {
        let nm = |e: u8, lead: bool, end: bool| matches!(e, b'\t' | b' ') && (lead || end);
        let body = encode(&data(SIZE), &nm);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=0badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.escapes.lead_dot.says(), Some(false));
        assert_eq!(lookalikes(&q), ["newsmangler"]);
    }

    #[test]
    fn nxg_upper_puts_size_in_ypart_and_escapes_del() {
        let nx = |e: u8, lead: bool, end: bool| (e == b' ' && (lead || end)) || e == 0x7f;
        let body = encode(&data(SIZE), &nx);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120 size=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=0BADF00D",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.ypart, "begin end size");
        assert_eq!(q.escapes.del.says(), Some(true));
        assert_eq!(lookalikes(&q), ["nxg-upper"]);
    }

    #[test]
    fn powerpost_has_no_total_and_long_lines() {
        // 128 input bytes a line: escapes make it longer
        let mut body = Vec::new();
        for chunk in data(SIZE).chunks(128) {
            let mut line = Vec::new();
            for b in chunk {
                let e = b.wrapping_add(42);
                if matches!(e, 0 | b'\n' | b'\r' | b'=') {
                    line.extend([b'=', e.wrapping_add(64)]);
                } else {
                    line.push(e);
                }
            }
            body.push(line);
        }
        let mut first = article(
            "=ybegin part=1 line=128 size=15360 name=a.mkv",
            None,
            body,
            false,
            "=yend size=5120 pcrc32=0badf00d",
        );
        first.push(Vec::new());
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.begin, "part line size name");
        assert_eq!(q.over_long.says(), Some(true));
        assert_eq!(q.blank_after_yend.says(), Some(true));
        assert_eq!(lookalikes(&q), ["PowerPost"]);
        assert_eq!(witness(&q, "PowerPost 2004").map(|w| w.0), Some(true));
    }

    #[test]
    fn yenc32_escapes_every_dot_and_keeps_the_drafts_space() {
        let y32 = |e: u8, _lead: bool, _end: bool| e == b'.';
        let body = encode(&data(SIZE), &y32);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv ",
            Some("=ypart begin=1 end=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=0BADF00D",
        );
        let q = Quirks::read(&first).unwrap();
        assert!(q.name_space);
        assert_eq!(q.escapes.mid_dot.says(), Some(true));
        assert_eq!(lookalikes(&q), ["yEnc32"]);
        assert_eq!(witness(&q, "yEnc32 1.0.8 - http://www.yenc32.com").map(|w| w.0), Some(true));
    }

    #[test]
    fn newspost_escapes_every_tab() {
        let np = |e: u8, lead: bool, _end: bool| e == b'\t' || (e == b'.' && lead);
        let body = encode(&data(SIZE), &np);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=0badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(q.escapes.mid_tab.says(), Some(true));
        assert_eq!(lookalikes(&q), ["newspost"]);
    }

    #[test]
    fn the_rust_crate_writes_no_total_and_escapes_no_whitespace() {
        let none = |_e: u8, _lead: bool, _end: bool| false;
        let body = encode(&data(SIZE), &none);
        let first = article(
            "=ybegin part=1 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            true,
            "=yend size=5120 part=1 pcrc32=0badf00d",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(lookalikes(&q), ["Rust yenc crate (pesto)"]);
    }

    #[test]
    fn a_tool_without_a_documented_yenc_gets_no_witness() {
        let q = nyuu();
        assert_eq!(witness(&q, "JBinUp 1.0"), None);
        assert_eq!(witness(&Quirks::default(), "Nyuu/0.4.2"), None, "nothing read");
        assert_eq!(class(&Quirks::default()), None);
    }

    #[test]
    fn a_claim_nothing_fits_says_why() {
        // Nyuu's shape with unpadded capitals: no documented tool writes that
        let body = encode(&data(SIZE), &rapid);
        let first = article(
            "=ybegin part=1 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=1 end=5120"),
            body,
            false,
            "=yend size=5120 part=1 pcrc32=BADF00D",
        );
        let q = Quirks::read(&first).unwrap();
        assert_eq!(class(&q), None);
        assert_eq!(witness(&q, "Nyuu/0.4.2"), Some((false, "yEnc quirks (capital CRCs, unpadded CRCs)".into())));
    }

    #[test]
    fn it_round_trips_and_old_probes_decode() {
        let q = nyuu();
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(serde_json::from_str::<Quirks>(&json).unwrap(), q);
        assert_eq!(serde_json::from_str::<Quirks>("{}").unwrap(), Quirks::default());
        assert_eq!(serde_json::to_string(&Quirks::default()).unwrap(), "{}");
    }

    #[test]
    fn only_a_part_that_says_so_is_the_last() {
        let body = encode(&data(SIZE), &rapid);
        // the last article indexed, but part 2 of 3: no crc32 is expected on it
        let middle = article(
            "=ybegin part=2 total=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=5121 end=10240"),
            body.clone(),
            false,
            "=yend size=5120 part=2 pcrc32=0badf00d",
        );
        let q = Quirks::read(&middle).unwrap();
        assert_eq!((q.yend.as_str(), q.last_yend.as_str()), ("size part pcrc32", ""));
        assert_eq!(witness(&q, "Nyuu/0.4.2").map(|w| w.0), Some(true));
        // no total=, but its end is the file's size
        let tail = article(
            "=ybegin part=3 line=128 size=15360 name=a.mkv",
            Some("=ypart begin=10241 end=15360"),
            body,
            false,
            "=yend size=5120 part=3 pcrc32=0badf00d",
        );
        assert_eq!(Quirks::read(&tail).unwrap().last_yend, "size part pcrc32");
    }

    #[test]
    fn malformed_lines_dont_panic() {
        assert_eq!(Quirks::read(&[b"plain text".to_vec()]), None);
        assert_eq!(Quirks::read(&[]), None);
        let odd: Vec<Vec<u8>> = vec![
            b"=ybegin ".to_vec(),
            b"=ypart".to_vec(),
            b"=".to_vec(),
            b"==".to_vec(),
            vec![0xff, b'=', 0xff, b'='],
            b"=yend".to_vec(),
        ];
        let q = Quirks::read(&odd).unwrap();
        assert!(q.begin.is_empty() && q.yend.is_empty());
        let _ = q.summary();
        let _ = lookalikes(&q);
        let junk: Vec<Vec<u8>> = vec![
            "=ybegin part==1 total line=x size=-5 name=".as_bytes().to_vec(),
            "=ypart begin= end=\u{1F600}".as_bytes().to_vec(),
            vec![b'='; 300],
            "=yend size=99999999999999999999999 pcrc32=zz crc32=123456789abc part=0".as_bytes().to_vec(),
            Vec::new(),
        ];
        let q = Quirks::read(&junk).unwrap();
        assert!(q.upper.is_empty() && q.padded.is_empty());
        let _ = witness(&q, "Nyuu");
        let _ = q.summary();
        // a line= at the top of the range, over two data lines
        let huge = [
            b"=ybegin part=1 total=2 line=18446744073709551615 size=9 name=x".to_vec(),
            b"abc".to_vec(),
            b"def".to_vec(),
            b"=yend size=6 part=1 pcrc32=00000001".to_vec(),
        ];
        assert_eq!(Quirks::read(&huge).unwrap().over_long.says(), Some(false));
        // the =ybegin with no line= and nothing after it
        let q = Quirks::read(&[b"=ybegin line=0 size=0 name=x".to_vec(), b"=yend size=0".to_vec()]).unwrap();
        assert_eq!(q.begin, "line size name");
    }
}
