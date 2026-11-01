//! How well a provider's candidate fits a title.
//!
//! Ported from newznab-tmux `app/Services/TvProcessing/Providers/AbstractTvProvider.php`, GPLv3,
//! stricter on year and country: a scene name carries those because the title clashes.

use super::title::Kind;

const MAX_COMPARED: usize = 200;

/// PHP similar_text: the longest common run, first found, plus the same on both sides of it.
pub fn similar(a: &[u8], b: &[u8]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (mut best, mut at_a, mut at_b) = (0, 0, 0);
    for i in 0..a.len() {
        for j in 0..b.len() {
            let run = a[i..].iter().zip(&b[j..]).take_while(|(x, y)| x == y).count();
            if run > best {
                (best, at_a, at_b) = (run, i, j);
            }
        }
    }
    if best == 0 {
        return 0;
    }
    best + similar(&a[..at_a], &b[..at_b]) + similar(&a[at_a + best..], &b[at_b + best..])
}

/// similar * 200 / (a.len() + b.len()). Inputs cut to 200 bytes.
pub fn percent(a: &str, b: &str) -> f64 {
    let (a, b) = (&a.as_bytes()[..a.len().min(MAX_COMPARED)], &b.as_bytes()[..b.len().min(MAX_COMPARED)]);
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    similar(a, b) as f64 * 200.0 / (a.len() + b.len()) as f64
}

/// `countries` empty means unknown. TMDB's `origin_country` can hold several.
pub struct Candidate<'a> {
    pub title: &'a str,
    pub year: Option<i32>,
    pub countries: &'a [&'a str],
}

/// `UK` is `GB`, otherwise uppercase.
pub fn country_code(c: &str) -> String {
    let upper = c.to_uppercase();
    if upper == "UK" { "GB".into() } else { upper }
}

/// 0.0 is no match. Compared lowercased. Exact 100, else percent if >= 75.
/// A country on our side against known countries, none equal under `country_code`: 0. Equal adds 2.
/// Both years known: within 1 adds 10, otherwise 0, even for an exact title.
/// Movie only: with no year on our side, only an exact title counts.
pub fn score(kind: Kind, ours: &str, our_year: Option<i32>, our_country: Option<&str>, c: &Candidate<'_>) -> f64 {
    let (a, b) = (ours.to_lowercase(), c.title.to_lowercase());
    let mut score = if a == b {
        100.0
    } else {
        if kind == Kind::Movie && our_year.is_none() {
            return 0.0;
        }
        let p = percent(&a, &b);
        if p < 75.0 {
            return 0.0;
        }
        p
    };
    if let Some(ours) = our_country
        && !c.countries.is_empty()
    {
        let ours = country_code(ours);
        if !c.countries.iter().any(|theirs| country_code(theirs) == ours) {
            return 0.0;
        }
        score += 2.0;
    }
    if let (Some(a), Some(b)) = (our_year, c.year) {
        if (a - b).abs() > 1 {
            return 0.0;
        }
        score += 10.0;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn similar_text_table() {
        assert!(close(percent("World", "Word"), 88.888));
        assert!(close(percent("some show us", "some show"), 85.714));
        assert!(close(percent("the office", "office"), 75.0));
        assert_eq!(percent("", ""), 0.0);
    }

    #[test]
    fn a_long_title_is_cut() {
        let (a, b) = ("ab".repeat(1 << 19), "ba".repeat(1 << 19));
        assert_eq!(percent(&a, &b), percent(&a[..200], &b[..200]));
    }

    #[test]
    fn score_table() {
        let c = |title, year, countries| Candidate { title, year, countries };
        type Row<'a> = (Kind, &'a str, Option<i32>, Option<&'a str>, Candidate<'a>, f64);
        let table: &[Row<'_>] = &[
            (Kind::Tv, "some show", None, None, c("Some Show", Some(2015), &[]), 100.0),
            (Kind::Tv, "some show", Some(2015), None, c("Some Show", Some(2016), &[]), 110.0),
            (Kind::Tv, "some show", Some(2015), None, c("Some Show", Some(1990), &[]), 0.0),
            (Kind::Tv, "some shows", Some(2015), None, c("Some Show", Some(1990), &[]), 0.0),
            (Kind::Tv, "some show", None, None, c("Another Thing", None, &[]), 0.0),
            (Kind::Tv, "some show", None, Some("UK"), c("Some Show", Some(2005), &["US"]), 0.0),
            (Kind::Tv, "some show", None, Some("UK"), c("Some Show", Some(2005), &["GB"]), 102.0),
            (Kind::Tv, "some show", None, Some("US"), c("Some Show", Some(2005), &[]), 100.0),
            (Kind::Tv, "some show", None, None, c("Some Show", Some(2005), &["GB"]), 100.0),
            (Kind::Tv, "some show", None, Some("US"), c("Some Show", None, &["CA", "US"]), 102.0),
            (Kind::Movie, "some movie", Some(2019), None, c("Some Movie", Some(2020), &[]), 110.0),
            (Kind::Movie, "some movie", Some(2019), None, c("Some Movie", Some(1985), &[]), 0.0),
            (Kind::Movie, "some movie", None, None, c("Some Movie", Some(1985), &[]), 100.0),
            (Kind::Movie, "some movies", None, None, c("Some Movie", Some(1985), &[]), 0.0),
        ];
        for (kind, ours, year, country, candidate, want) in table {
            assert_eq!(score(*kind, ours, *year, *country, candidate), *want, "{ours} {:?}", candidate.title);
        }
    }

    #[test]
    fn country_code_table() {
        assert_eq!(country_code("UK"), "GB");
        assert_eq!(country_code("uk"), "GB");
        assert_eq!(country_code("GB"), "GB");
        assert_eq!(country_code("us"), "US");
    }
}
