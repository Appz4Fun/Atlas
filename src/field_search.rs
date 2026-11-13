//! Shared native field selection, count formatting, and literal filter helpers.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields {
    pub group: String,
    pub poster: String,
    pub email: String,
    pub size: String,
    pub team: String,
    /// the file's CRC32 in hex, from a probe
    pub crc: String,
    /// the posting tool, from a probe
    pub agent: String,
}

impl Fields {
    pub fn any(&self) -> bool {
        self.chosen().next().is_some()
    }

    /// What's asked, under the words a page uses.
    pub fn chosen(&self) -> impl Iterator<Item = (&'static str, String)> + '_ {
        [
            ("Usenet group", &self.group),
            ("Poster", &self.poster),
            ("Email", &self.email),
            ("Size", &self.size),
            ("Release group", &self.team),
            ("CRC32", &self.crc),
            ("Posted with", &self.agent),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name, value.clone()))
    }

    /// The same, without what `name` of `chosen` stands for.
    pub fn without(&self, name: &str) -> Fields {
        let mut out = self.clone();
        match name {
            "Usenet group" => out.group.clear(),
            "Poster" => out.poster.clear(),
            "Email" => out.email.clear(),
            "Size" => out.size.clear(),
            "Release group" => out.team.clear(),
            "CRC32" => out.crc.clear(),
            _ => out.agent.clear(),
        }
        out
    }

    /// `name=value` for a url.
    pub fn query(&self) -> Vec<String> {
        let encode = |s: &str| {
            let mut out = String::new();
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            out
        };
        [
            ("group", &self.group),
            ("poster", &self.poster),
            ("email", &self.email),
            ("size", &self.size),
            ("team", &self.team),
            ("crc", &self.crc),
            ("agent", &self.agent),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect()
    }
}

/// How many releases a search has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Total {
    pub count: i64,
    /// the whole database was looked at, and no shard hit the limit
    pub exact: bool,
}

impl Total {
    pub fn describe(&self) -> String {
        let mut digits = String::new();
        let text = self.count.to_string();
        for (i, c) in text.chars().enumerate() {
            if i > 0 && (text.len() - i).is_multiple_of(3) {
                digits.push(',');
            }
            digits.push(c);
        }
        if self.count == 0 && !self.exact {
            return "None in the newest releases".into();
        }
        let noun = if self.count == 1 { "release" } else { "releases" };
        if self.exact { format!("{digits} {noun}") } else { format!("At least {digits} {noun}") }
    }
}

pub(crate) fn size_range(text: &str) -> Option<(i64, i64)> {
    let lower = text.to_ascii_lowercase();
    let digits = lower.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let (number, unit) = lower.split_at(digits);
    let scale: f64 = match unit.trim_end_matches("ib").trim_end_matches('b') {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        "t" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let value: f64 = number.parse().ok()?;
    let decimals = number.split_once('.').map_or(0, |(_, after)| after.len());
    let half = 0.5 / 10f64.powi(decimals as i32);
    Some((((value - half) * scale).ceil() as i64, ((value + half) * scale).floor() as i64))
}

pub(crate) fn team_pattern(team: &str) -> String {
    team.replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_")
}

pub(crate) fn team_condition(name: &str, param: &str) -> String {
    let shapes: Vec<String> = ["'%-'", "'%- '"]
        .into_iter()
        .flat_map(|head| ["''", "'.%'", "' %'", "'[%'"].map(|tail| (head, tail)))
        .map(|(head, tail)| format!(r"{name} like {head} || {param} || {tail} escape '\'"))
        .collect();
    format!("({})", shapes.join(" or "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_name_what_is_asked() {
        let f = Fields { email: "a@b.c".into(), size: "12".into(), ..Fields::default() };
        assert!(f.any());
        assert_eq!(f.query(), ["email=a%40b.c", "size=12"]);
        assert_eq!(f.without("Email").query(), ["size=12"]);
        assert!(!Fields::default().any());
    }

    #[test]
    fn a_total_says_when_it_is_only_part() {
        assert_eq!(Total { count: 1234, exact: true }.describe(), "1,234 releases");
        assert_eq!(Total { count: 1, exact: true }.describe(), "1 release");
        assert_eq!(Total { count: 2000, exact: false }.describe(), "At least 2,000 releases");
        assert_eq!(Total { count: 0, exact: false }.describe(), "None in the newest releases");
    }

    #[test]
    fn a_written_size_finds_what_is_written_that_way() {
        let gib = 1024 * 1024 * 1024_i64;
        let (low, high) = size_range("1.5GB").unwrap();
        assert!(low <= gib * 3 / 2 && gib * 3 / 2 <= high);
        assert_eq!(crate::ui::fmt_size(Some(low)), "1.5 GB");
        assert_eq!(crate::ui::fmt_size(Some(high)), "1.5 GB");
        assert_eq!(crate::ui::fmt_size(Some(low - 1)), "1.4 GB");
        assert_eq!(crate::ui::fmt_size(Some(high + 2)), "1.6 GB");
        assert_eq!(size_range("700mib"), size_range("700M"));
        assert_eq!(size_range("12x"), None);
        assert_eq!(size_range("GB"), None);
    }

    #[test]
    fn a_size_that_is_no_number_has_no_range() {
        assert_eq!(size_range("abc"), None);
    }

    #[test]
    fn team_patterns_escape_like_metacharacters_without_changing_literal_punctuation() {
        for (team, expected) in [
            ("SubsPlease", "SubsPlease"),
            ("100%_TEAM", r"100\%\_TEAM"),
            (r"A\B", r"A\\B"),
            (r"A%_B\C/'D", r"A\%\_B\\C/'D"),
            ("x' OR true --", "x' OR true --"),
        ] {
            assert_eq!(team_pattern(team), expected, "literal team {team:?}");
        }
    }

    #[test]
    #[ignore = "requires an explicitly admitted isolated PostgreSQL schema"]
    fn native_team_filter_matches_the_parsed_bracket_prefix_and_literal_suffixes() {
        use crate::{parser::{Article, Release}, pg};
        use std::time::{Duration, Instant};
        let schema = pg::testing::Schema::native_writer().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let mut client = schema.pool.get().unwrap();
        let names = [
            "[SubsPlease] Some Anime - 05 (1080p) [ABCDEF12].mkv",
            "[subsplease] Second Anime.mkv",
            "[SubsPlease]Adjacent.Title.mkv",
            "Film.One-SubsPlease",
            "Film.Two- SubsPlease",
            "Film.Three-SubsPlease.mkv",
            "Film.Four- SubsPlease.mkv",
            "Film.Five-SubsPlease bonus",
            "Film.Six- SubsPlease bonus",
            "Film.Seven-SubsPlease[ABCDEF12]",
            "Film.Eight- SubsPlease[ABCDEF12]",
            "[SubsPlease2] Wrong.Team.mkv",
            "[Subs] Wrong.Partial.mkv",
            "Title [SubsPlease] Wrong.Position.mkv",
            "[SubsPlease Missing.Closing.Bracket.mkv",
            "Film.Nine-SubsPlease2.mkv",
            r"[A%_B\C/'D] Literal.Prefix.mkv",
            r"Film.Ten-A%_B\C/'D.mkv",
            r"[AxxzB\C/'D] Wrong.Wildcard.mkv",
            "[A%_BC/'D] Wrong.Escape.mkv",
            "[ÄBC] Upper.NonASCII.mkv",
            "[äbc] Lower.NonASCII.mkv",
            "[x' OR true --] Literal.Quote.mkv",
        ];
        let mut ids = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let saved = pg::release_writer::save(&mut client, &[Release {
                name: (*name).into(), group: "alt.binaries.test".into(), poster: format!("poster-{index}"),
                date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles: vec![Article {
                    filename: Some("video.mkv".into()), subject: "video.mkv yEnc (1/1)".into(),
                    message_id: format!("<team-filter-{index}@example.invalid>"),
                    part: Some(1), total_parts: Some(1), bytes: 64, ..Default::default()
                }], ..Default::default()
            }], 1).unwrap();
            assert_eq!((saved.releases, saved.files, saved.parts), (1, 1, 1));
            ids.push(client.query_one("select id from releases where name=$1", &[name]).unwrap().get::<_,i64>(0));
        }
        // This is the same parser group used to build the release's team link.
        let parsed_team = crate::release_tags::parse(names[0]).group.unwrap();
        assert_eq!(parsed_team, "SubsPlease");
        for (team, wanted) in [
            (parsed_team.as_str(), vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            ("SUBSPLEASE", vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            (r"A%_B\C/'D", vec![16, 17]),
            ("Äbc", vec![20]),
            ("x' OR true --", vec![22]),
        ] {
            let fields = Fields { team: team.into(), ..Default::default() };
            let view = pg::fresh_reader::ReadView::open(&mut client, &schema.name, Default::default(), Instant::now() + Duration::from_secs(10)).unwrap();
            let (rows, total) = view.field_search(&fields, "", None, 0, 50).unwrap();
            assert_eq!(total, wanted.len() as u64, "team {team:?}");
            let expected = wanted.into_iter().rev().map(|index| ids[index]).collect::<Vec<_>>();
            assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), expected, "team {team:?}");
        }
        // Team shares one bound parameter after other fields and before words.
        let fields = Fields { group: "alt.binaries.test".into(), poster: "poster-0".into(), size: "64".into(),
            team: parsed_team, ..Default::default() };
        let view = pg::fresh_reader::ReadView::open(&mut client, &schema.name, Default::default(), Instant::now() + Duration::from_secs(10)).unwrap();
        let (rows, total) = view.field_search(&fields, "Anime", None, 0, 50).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [ids[0]]);
    }

}
