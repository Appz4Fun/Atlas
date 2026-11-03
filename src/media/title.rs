//! A title's stable key: the same show whatever case or punctuation it's written in.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Tv = 1,
    Movie = 2,
}

static COUNTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.+?)\s+(US|UK|AU|NZ|CA|NL)$").unwrap());

/// Transliterate as upstream does; `&` to ` and `; `$` to `s`; drop apostrophes; lowercase
/// alphanumerics; everything else a space; collapse.
pub fn normalize(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.chars() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'æ' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' => out.push('a'),
            'ç' | 'Ç' => out.push('c'),
            'Σ' | 'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => out.push('i'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'Ú' | 'Û' | 'Ü' | 'Ū' => out.push('u'),
            'ß' => out.push_str("ss"),
            '&' => out.push_str(" and "),
            '$' => out.push('s'),
            '\'' | '’' => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a 64 of "tv|title|year" or "movie|title|year" (year empty when unknown), shifted right once.
pub fn title_key(kind: Kind, normalized: &str, year: Option<i32>) -> i64 {
    let label = match kind {
        Kind::Tv => "tv",
        Kind::Movie => "movie",
    };
    let year = year.map_or(String::new(), |y| y.to_string());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{label}|{normalized}|{year}").bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash >> 1) as i64
}

/// `Some Show US` gives (`Some Show`, Some("US")).
pub fn split_country(title: &str) -> (&str, Option<&str>) {
    match COUNTRY.captures(title) {
        Some(c) => match (c.get(1), c.get(2)) {
            (Some(name), Some(country)) => (name.as_str(), Some(country.as_str())),
            _ => (title, None),
        },
        None => (title, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_table() {
        assert_eq!(normalize("Détective & Privé: The $tory's End!"), "detective and prive the storys end");
        assert_eq!(normalize("S.W.A.T"), "s w a t");
        assert_eq!(normalize("Some   Show"), "some show");
        assert_eq!(normalize("ß"), "ss");
    }

    #[test]
    fn title_keys_are_pinned() {
        assert_eq!(title_key(Kind::Tv, "some show", None), 2517964955805247441);
        assert_eq!(title_key(Kind::Tv, "some show", Some(2019)), 3994912364065961063);
        assert_eq!(title_key(Kind::Movie, "some movie", Some(2019)), 7250957560060540805);
        assert_eq!(title_key(Kind::Movie, "some movie", None), 3314878267631545165);
    }

    #[test]
    fn split_country_table() {
        assert_eq!(split_country("Some Show US"), ("Some Show", Some("US")));
        assert_eq!(split_country("USA Today"), ("USA Today", None));
        assert_eq!(split_country("Some Show"), ("Some Show", None));
    }
}
