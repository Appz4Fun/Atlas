//! An AlloCiné id in place of a title. French groups name a release after its AlloCiné page:
//! `AC31594.2024.S02E05.Love.and.Be.Loved.MULTi…-BYOR` is series 31594 there, Dark Matter, and
//! `AC317772.2026.MULTi.VFF…-FRQC` is film 317772, Matchbox: The Movie. Films and series are numbered apart, soo the
//! kind the name parses as says which page it is.
//!
//! Wikidata keeps AlloCiné's ids beside IMDb's, TMDB's and TVDB's, and answers most with no search at all. For the
//! rest, the AlloCiné page has the title and the year, and the providers search by those.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::config::AllocineConfig;

use super::http::{Client, Memo, Reply, encode};
use super::query::MAX_TITLE;
use super::resolve::{Ids, Trouble, imdb_of, int_of};
use super::title::Kind;

const WIKIDATA: &str = "https://query.wikidata.org/sparql";
const SITE: &str = "https://www.allocine.fr";

static AC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^AC([0-9]{4,10})$").unwrap());
static LD_JSON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<script[^>]*type="application/ld\+json"[^>]*>(.*?)</script>"#).unwrap());
static PAGE_TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<title>([^<]*)</title>").unwrap());
/// `Dark Matter - Série TV 2024 - AlloCiné`: the year is the last before the site's name
static TITLE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" ((?:19|20)[0-9]{2}) - AlloCin").unwrap());
static ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]{1,7}|#x[0-9a-fA-F]{1,6}|[a-z]{2,6});").unwrap());

/// The AlloCiné id a parsed title is, if it's nothing else: `AC31594`, not `ACME` or `AC 31594`.
pub fn allocine_id(title: &str) -> Option<u64> {
    AC.captures(title).and_then(|c| c[1].parse().ok()).filter(|id| *id > 0)
}

/// What an AlloCiné id is: a title to search by, and whatever ids Wikidata had for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    pub title: String,
    pub year: Option<i32>,
    pub ids: Ids,
}

pub struct Allocine {
    wikidata_url: String,
    site: String,
    wikidata: Client,
    pages: Client,
    decoded: Memo<Option<Decoded>>,
}

impl Allocine {
    pub fn new(cfg: &AllocineConfig) -> Allocine {
        let wikidata_url = cfg.wikidata_url.clone().unwrap_or_else(|| WIKIDATA.to_string());
        let site = cfg.base_url.clone().unwrap_or_else(|| SITE.to_string());
        Allocine {
            wikidata: Client::new("wikidata", &wikidata_url, cfg.requests_per_minute),
            pages: Client::new("allocine", &site, cfg.requests_per_minute),
            wikidata_url,
            site,
            decoded: Memo::new(),
        }
    }

    #[doc(hidden)]
    pub fn clients(&self) -> [&Client; 2] {
        [&self.wikidata, &self.pages]
    }

    /// None: neither knows this id as this kind.
    pub fn decode(&self, kind: Kind, id: u64) -> Result<Option<Decoded>, Trouble> {
        let key = format!("{kind:?}|{id}");
        if let Some(known) = self.decoded.get(&key) {
            return Ok(known);
        }
        let (ids, label) = self.wikidata(kind, id)?;
        let page = if label.is_some() && ids.any() { None } else { self.page(kind, id)? };
        let decoded = match (page, label) {
            (Some((title, year)), _) => Some(Decoded { title, year, ids }),
            (None, Some(title)) => Some(Decoded { title, year: None, ids }),
            // ids with no name: the providers still have to be asked something, and it's soo they find nothing
            (None, None) if ids.any() => Some(Decoded { title: format!("AC{id}"), year: None, ids }),
            (None, None) => None,
        };
        // a miss isn't kept here: the title's own miss waits its time, and Wikidata may have it by then
        if decoded.is_some() {
            self.decoded.put(&key, decoded.clone());
        }
        Ok(decoded)
    }

    /// The ids of the one item with this AlloCiné id, and its English name.
    fn wikidata(&self, kind: Kind, id: u64) -> Result<(Ids, Option<String>), Trouble> {
        let (allocine, tmdb, tvdb) = match kind {
            Kind::Movie => ("P1265", "P4947", ""),
            Kind::Tv => ("P1267", "P4983", "OPTIONAL { ?item wdt:P4835 ?tvdb } "),
        };
        let query = format!(
            "SELECT ?item ?label ?imdb ?tmdb ?tvdb WHERE {{ ?item wdt:{allocine} \"{id}\" . \
             OPTIONAL {{ ?item wdt:P345 ?imdb }} OPTIONAL {{ ?item wdt:{tmdb} ?tmdb }} {tvdb}\
             OPTIONAL {{ ?item rdfs:label ?label . FILTER(lang(?label) = \"en\") }} }} LIMIT 20"
        );
        let url = format!("{}?format=json&query={}", self.wikidata_url, encode(&query));
        let Reply::Json(body) = self.wikidata.get(&url, &[("Accept", "application/sparql-results+json")])? else {
            return Ok((Ids::default(), None));
        };
        Ok(wikidata_answer(&body))
    }

    /// The page's title, original where it gives one, and its year.
    fn page(&self, kind: Kind, id: u64) -> Result<Option<(String, Option<i32>)>, Trouble> {
        let url = match kind {
            Kind::Movie => format!("{}/film/fichefilm_gen_cfilm={id}.html", self.site),
            Kind::Tv => format!("{}/series/ficheserie_gen_cserie={id}.html", self.site),
        };
        // what was taken from the page, not the page: a few bytes in place of a third of a megabyte
        if let Some((200, kept)) = crate::raw_store::get_http_cache("allocine", &url)
            && let Ok(kept) = serde_json::from_str::<Value>(&kept)
            && let Some(title) = kept.get("title").and_then(Value::as_str)
        {
            let year = kept.get("year").and_then(Value::as_i64).and_then(|y| i32::try_from(y).ok());
            return Ok(Some((title.to_string(), year)));
        }
        let Some(html) = self.pages.get_page(&url)? else { return Ok(None) };
        let found = page_answer(kind, &html);
        if let Some((title, year)) = &found {
            let kept = serde_json::json!({ "title": title, "year": year });
            crate::raw_store::put_http_cache("allocine", &url, 200, &kept.to_string());
        }
        Ok(found)
    }
}

/// A value only when every row that has it agrees.
fn agreed<T: PartialEq>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut out: Option<T> = None;
    for v in values {
        match &out {
            None => out = Some(v),
            Some(seen) if *seen == v => {}
            Some(_) => return None,
        }
    }
    out
}

/// SPARQL JSON results: one row for each mix of values. Two items with the id is no answer, as is none.
pub fn wikidata_answer(body: &Value) -> (Ids, Option<String>) {
    let rows: Vec<&Value> =
        body.pointer("/results/bindings").and_then(Value::as_array).map_or_else(Vec::new, |r| r.iter().collect());
    let field = |row: &Value, name: &str| row.get(name).and_then(|v| v.get("value")).cloned();
    if agreed(rows.iter().filter_map(|r| field(r, "item"))).is_none() {
        return (Ids::default(), None);
    }
    let ids = Ids {
        imdb: agreed(rows.iter().filter_map(|r| field(r, "imdb")).filter_map(|v| imdb_of(&v))),
        tmdb: agreed(rows.iter().filter_map(|r| field(r, "tmdb")).filter_map(|v| int_of(&v))),
        tvdb: agreed(rows.iter().filter_map(|r| field(r, "tvdb")).filter_map(|v| int_of(&v))),
        ..Ids::default()
    };
    let label = agreed(rows.iter().filter_map(|r| field(r, "label")).filter_map(|v| v.as_str().map(clean)))
        .filter(|t| usable(t));
    (ids, label)
}

/// The page's JSON-LD names the film or series. A page of the other kind is no answer.
pub fn page_answer(kind: Kind, html: &str) -> Option<(String, Option<i32>)> {
    let wanted = match kind {
        Kind::Movie => "Movie",
        Kind::Tv => "TVSeries",
    };
    let thing = LD_JSON
        .captures_iter(html)
        .filter_map(|c| serde_json::from_str::<Value>(c[1].trim()).ok())
        .flat_map(|v| match v {
            Value::Array(all) => all,
            one => vec![one],
        })
        .find(|v| v.get("@type").and_then(Value::as_str) == Some(wanted))?;
    let text = |key: &str| thing.get(key).and_then(Value::as_str).map(clean).filter(|t| usable(t));
    // the original title where it's given: what the providers know it by
    let title = text("alternateName").or_else(|| text("name"))?;
    let year =
        PAGE_TITLE.captures(html).and_then(|c| TITLE_YEAR.captures_iter(&c[1]).last().and_then(|y| y[1].parse().ok()));
    Some((title, year))
}

fn usable(title: &str) -> bool {
    !title.is_empty() && title.len() <= MAX_TITLE
}

/// Entities as the page writes them, and spaces collapsed.
fn clean(text: &str) -> String {
    let decoded = ENTITY.replace_all(text, |c: &regex::Captures<'_>| {
        let name = &c[1];
        let code = if let Some(hex) = name.strip_prefix("#x") {
            u32::from_str_radix(hex, 16).ok()
        } else if let Some(dec) = name.strip_prefix('#') {
            dec.parse().ok()
        } else {
            match name {
                "amp" => Some(38),
                "quot" => Some(34),
                "apos" => Some(39),
                "lt" => Some(60),
                "gt" => Some(62),
                "nbsp" => Some(32),
                _ => None,
            }
        };
        code.and_then(char::from_u32).map_or_else(|| c[0].to_string(), String::from)
    });
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_bare_ac_number_is_an_id() {
        assert_eq!(allocine_id("AC31594"), Some(31594));
        assert_eq!(allocine_id("AC1000000194"), Some(1_000_000_194));
        assert_eq!(allocine_id("ACME"), None);
        assert_eq!(allocine_id("AC 31594"), None);
        assert_eq!(allocine_id("AC12"), None);
        assert_eq!(allocine_id("ac31594"), None);
        assert_eq!(allocine_id("AC0000"), None);
        assert_eq!(allocine_id("AC31594 Show"), None);
    }

    fn row(item: &str, extra: &[(&str, &str)]) -> Value {
        let mut r = json!({ "item": { "type": "uri", "value": item } });
        for (k, v) in extra {
            r[*k] = json!({ "type": "literal", "value": v });
        }
        r
    }

    #[test]
    fn one_item_gives_its_ids() {
        let body = json!({ "results": { "bindings": [row("Q1", &[
            ("imdb", "tt34866681"), ("tmdb", "278624"), ("tvdb", "457437"), ("label", "Lucky")
        ])] } });
        let (ids, label) = wikidata_answer(&body);
        assert_eq!(ids, Ids { imdb: Some(34_866_681), tmdb: Some(278_624), tvdb: Some(457_437), ..Ids::default() });
        assert_eq!(label.as_deref(), Some("Lucky"));
    }

    #[test]
    fn two_items_or_two_values_are_no_answer() {
        let two = json!({ "results": { "bindings": [row("Q1", &[("imdb", "tt1")]), row("Q2", &[("imdb", "tt2")])] } });
        assert_eq!(wikidata_answer(&two), (Ids::default(), None));
        let split = json!({ "results": { "bindings": [
            row("Q1", &[("imdb", "tt1111111"), ("tmdb", "5")]), row("Q1", &[("imdb", "tt1111111"), ("tmdb", "6")])
        ] } });
        assert_eq!(wikidata_answer(&split).0, Ids { imdb: Some(1_111_111), ..Ids::default() });
        assert_eq!(wikidata_answer(&json!({ "results": { "bindings": [] } })), (Ids::default(), None));
    }

    fn page(ld: &str, title: &str) -> String {
        format!("<html><head><title>{title}</title><script type=\"application/ld+json\">{ld}</script></head></html>")
    }

    #[test]
    fn a_page_gives_the_original_title_and_the_year() {
        let film = page(
            r#"{"@context":"http://schema.org","@type":"Movie","name":"Matchbox — Le Film","alternateName":"Matchbox The Movie"}"#,
            "Matchbox — Le Film - Film 2026 - AlloCiné",
        );
        assert_eq!(page_answer(Kind::Movie, &film), Some(("Matchbox The Movie".into(), Some(2026))));
        let show = page(r#"{"@type":"TVSeries","name":"Dark Matter"}"#, "Dark Matter - Série TV 2024 - AlloCiné");
        assert_eq!(page_answer(Kind::Tv, &show), Some(("Dark Matter".into(), Some(2024))));
    }

    #[test]
    fn a_page_of_the_other_kind_is_no_answer() {
        let film = page(r#"{"@type":"Movie","name":"Rapaces"}"#, "Rapaces - Film 2025 - AlloCiné");
        assert_eq!(page_answer(Kind::Tv, &film), None);
        assert_eq!(page_answer(Kind::Movie, "<html>nothing here</html>"), None);
    }

    #[test]
    fn entities_are_decoded() {
        let film = page(r#"{"@type":"Movie","name":"L&#039;Homme &amp; la Mer"}"#, "x - Film 1999 - AlloCiné");
        assert_eq!(page_answer(Kind::Movie, &film), Some(("L'Homme & la Mer".into(), Some(1999))));
    }
}

#[cfg(test)]
mod names {
    use super::allocine_id;
    use crate::media::parse::{parse_movie, parse_tv};

    #[test]
    fn the_names_seen_parse_to_their_ids() {
        let tv = [
            ("AC1000000194.2026.S01E04.MULTi.VFF.DV.HDR.2160p.WEB.AC3.5.1.H265-FRQC", 1_000_000_194, Some(2026)),
            ("AC30890.S04E01.MULTi.1080p.WEB.H264-GL0P", 30_890, None),
            ("AC31594.2024.S02E05.Love.and.Be.Loved.MULTi.AD.2160p.ATV.WEB.DV.HDR.H265-BYOR", 31_594, Some(2024)),
        ];
        for (name, id, year) in tv {
            let p = parse_tv(name).unwrap();
            assert_eq!((allocine_id(&p.title), p.year), (Some(id), year), "{name}");
        }
        let films = [
            ("AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC", 317_772),
            ("AC267335.2026.FRENCH.CA.1080p.WEB.H264-SUPPLY", 267_335),
        ];
        for (name, id) in films {
            let p = parse_movie(name).unwrap();
            assert_eq!((allocine_id(&p.title), p.year), (Some(id), Some(2026)), "{name}");
        }
    }
}
