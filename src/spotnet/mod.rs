//! Spotnet "spots": signed posts that describe a release (title, category, size, an IMDb link) and point at a
//! compressed NZB for it. Spots are posted as header-only articles in `free.pt`; the spot's XML rides in its
//! `X-XML` headers, and its NZB is a deflated blob posted in `alt.binaries.ftd`.
//!
//! `header`, `full` and `binary` read the formats: the overview line, the full spot XML and the posted binaries.
//! `release` turns a spot's NZB into a release, `store` keeps the bookkeeping and the tags, `ingest` is the
//! indexer's task that reads new spots, `comments` keeps what's said about them (comments, spam reports),
//! `names` says what a spot's subcategories mean, `ubb` renders a spot's or comment's text, and `sign` checks a
//! spot's signatures.

pub mod binary;
pub mod blacklist;
pub mod category;
pub mod comments;
pub mod full;
pub mod header;
pub mod ingest;
pub mod names;
pub mod release;
pub mod sign;
pub mod store;
pub mod ubb;

/// Where spots are posted.
pub const SPOT_GROUP: &str = "free.pt";
/// Where a spot's NZB and image are posted.
pub const NZB_GROUP: &str = "alt.binaries.ftd";

/// The longest spot XML read: Spotweb skips larger spots as spam, and so does this.
pub const MAX_XML_BYTES: usize = 50 * 1024;

/// What a spot is, for the categories Atlas takes: movies and TV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Series,
}

impl Kind {
    /// The kind a `spots` row's `kind` names, if any.
    pub fn parse(kind: Option<&str>) -> Option<Kind> {
        match kind? {
            "movie" => Some(Kind::Movie),
            "series" => Some(Kind::Series),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Movie => "movie",
            Kind::Series => "series",
        }
    }
}

/// A spot's subcategories, by letter. Values are the numbers after the letter, as Spotweb keeps them
/// (`a9`, `b4`, `z0`, ...). For video (`category` 0): `a` is the format, `b` the source, `c` the language,
/// `d` the genre and `z` the kind (0 movie, 1 series, 2 book, 3 erotica, 4 pictures).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subcats {
    pub a: Vec<u16>,
    pub b: Vec<u16>,
    pub c: Vec<u16>,
    pub d: Vec<u16>,
    pub z: Vec<u16>,
}

/// Genres (`d`) that mark a video spot as erotica, whatever its `z` says (Spotweb's `createSubcatZ`).
const EROTIC_GENRES: [u16; 22] =
    [23, 24, 25, 26, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89];

impl Subcats {
    /// Adds one subcategory, `letter` lowercase.
    pub fn push(&mut self, letter: char, value: u16) {
        let list = match letter {
            'a' => &mut self.a,
            'b' => &mut self.b,
            'c' => &mut self.c,
            'd' => &mut self.d,
            'z' => &mut self.z,
            _ => return,
        };
        if !list.contains(&value) {
            list.push(value);
        }
    }

    /// The kind a spot of `category` with these subcategories has, when it is one Atlas takes. A spot that lists
    /// no `z` gets the one Spotweb would give it: erotica for an erotic genre, series for a TV source or genre,
    /// books and pictures for their formats, a movie otherwise.
    pub fn kind(&self, category: i32) -> Option<Kind> {
        if category != 0 || self.d.iter().any(|d| EROTIC_GENRES.contains(d)) {
            return None;
        }
        let z = match self.z.first() {
            Some(&z) => z,
            None if self.b.contains(&4) || self.d.contains(&11) => 1,
            None if self.a.iter().any(|a| [5, 11, 12, 13].contains(a)) => 2,
            None => 0,
        };
        match z {
            0 => Some(Kind::Movie),
            1 => Some(Kind::Series),
            _ => None,
        }
    }

    /// Spotweb's form: `a9|b4|b11|` and so on, letters in order.
    pub fn spotweb(&self) -> String {
        let mut out = String::new();
        for (letter, list) in [('a', &self.a), ('b', &self.b), ('c', &self.c), ('d', &self.d), ('z', &self.z)] {
            for v in list {
                out.push(letter);
                out.push_str(&v.to_string());
                out.push('|');
            }
        }
        out
    }
}

/// Whether a message-id from a spot can be asked for: something@something, with nothing that would break the
/// command line or the brackets around it.
pub fn valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 250
        && id.contains('@')
        && !id.bytes().any(|b| b == b'<' || b == b'>' || b.is_ascii_whitespace() || b.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subcats(list: &[(char, u16)]) -> Subcats {
        let mut s = Subcats::default();
        for &(l, v) in list {
            s.push(l, v);
        }
        s
    }

    #[test]
    fn kinds() {
        assert_eq!(subcats(&[('a', 9), ('z', 0)]).kind(0), Some(Kind::Movie));
        assert_eq!(subcats(&[('a', 9), ('b', 4), ('z', 1)]).kind(0), Some(Kind::Series));
        // music, games and apps are other categories
        assert_eq!(subcats(&[('z', 0)]).kind(1), None);
        assert_eq!(subcats(&[('z', 0)]).kind(2), None);
        // books, erotica and pictures
        assert_eq!(subcats(&[('z', 2)]).kind(0), None);
        assert_eq!(subcats(&[('z', 3)]).kind(0), None);
        assert_eq!(subcats(&[('z', 4)]).kind(0), None);
        // an erotic genre wins over a movie z
        assert_eq!(subcats(&[('d', 75), ('z', 0)]).kind(0), None);
    }

    #[test]
    fn kind_without_z() {
        assert_eq!(subcats(&[('a', 9), ('d', 5)]).kind(0), Some(Kind::Movie));
        assert_eq!(subcats(&[('a', 9), ('b', 4)]).kind(0), Some(Kind::Series));
        assert_eq!(subcats(&[('d', 11)]).kind(0), Some(Kind::Series));
        assert_eq!(subcats(&[('a', 5)]).kind(0), None);
        assert_eq!(subcats(&[('d', 23)]).kind(0), None);
    }

    #[test]
    fn spotweb_form() {
        assert_eq!(subcats(&[('z', 1), ('a', 9), ('b', 4), ('b', 11), ('b', 4)]).spotweb(), "a9|b4|b11|z1|");
    }

    #[test]
    fn message_ids() {
        assert!(valid_message_id("YBUZv8CCBsIFzvJYg7Xea@spot.net"));
        assert!(!valid_message_id(""));
        assert!(!valid_message_id("no-at-sign"));
        assert!(!valid_message_id("<a@b>"));
        assert!(!valid_message_id("a b@c"));
    }
}
