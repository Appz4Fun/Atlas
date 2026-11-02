//! Searching on what isn't in the name search: Usenet group, poster, email, exact size, release group.
//!
//! `group_name` has an index. `poster` and `size` get theirs from `atlas --search-indexes`, a one-off because
//! building them takes a while on a big database. Until then a search looks at the newest `SCAN_LIMIT` releases
//! of each shard, and says so in its count, soo a page is never left waiting on a whole shard.

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::category::CatFilter;
use crate::db::{self, Result};
use crate::search::{COLUMNS, ReleaseRow, VISIBLE_RELEASE, like_query, release_row};
use crate::store;

const SCAN_LIMIT: i64 = 200_000;
/// Counting stops here in each shard.
const COUNT_LIMIT: i64 = 1_000_000;
const POSTER_INDEX: &str = "idx_release_poster";
const SIZE_INDEX: &str = "idx_release_size";

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

fn has_index(conn: &Connection, shard: usize, name: &str) -> bool {
    conn.query_row(&format!("select 1 from s{shard}.sqlite_master where type = 'index' and name = ?"), [name], |_| {
        Ok(())
    })
    .is_ok()
}

/// One shard's search: its `from` clause, and whether that covers the whole shard.
/// `1.5 GB`, as the pages write sizes, in binary units: every size that would be written that way, soo a size read off
/// a page finds its release. Spaces and commas are already gone.
fn size_range(text: &str) -> Option<(i64, i64)> {
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

struct Plan {
    conditions: Vec<String>,
    values: Vec<Value>,
}

fn plan(fields: &Fields, words: &str, filter: Option<&CatFilter>) -> Plan {
    let mut p = Plan { conditions: vec![VISIBLE_RELEASE.to_string()], values: Vec::new() };
    let add = |p: &mut Plan, condition: &str, value: Value| {
        p.values.push(value);
        p.conditions.push(condition.replace("{}", &format!("?{}", p.values.len() + 2)));
    };
    if !fields.group.is_empty() {
        add(&mut p, "r.group_name = {}", Value::Text(fields.group.clone()));
    }
    if !fields.poster.is_empty() {
        add(&mut p, "r.poster = {}", Value::Text(fields.poster.clone()));
    }
    if !fields.email.is_empty() {
        add(&mut p, "@email:{}", Value::Text(like_query(&fields.email)));
    }
    if let Ok(size) = fields.size.parse::<i64>() {
        add(&mut p, "r.size = {}", Value::Integer(size));
    } else if let Some((low, high)) = size_range(&fields.size) {
        add(&mut p, "r.size >= {}", Value::Integer(low));
        add(&mut p, "r.size <= {}", Value::Integer(high));
    } else if !fields.size.is_empty() {
        p.conditions.push("0".into());
    }
    if !fields.team.is_empty() {
        let team = fields.team.replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_");
        // at the end of the name, or before an extension or a tag, with PTT's space after the hyphen allowed
        let shapes: Vec<String> = ["'%-'", "'%- '"]
            .into_iter()
            .flat_map(|head| ["''", "'.%'", "' %'", "'[%'"].map(|tail| (head, tail)))
            .map(|(head, tail)| format!(r"coalesce(r.display_name, r.name) like {head} || {{}} || {tail} escape '\'"))
            .collect();
        add(&mut p, &format!("({})", shapes.join(" or ")), Value::Text(team));
    }
    if let Ok(crc) = u32::from_str_radix(fields.crc.trim_start_matches("0x"), 16) {
        add(&mut p, "@probe:crc32:{}", Value::Integer(i64::from(crc)));
    } else if !fields.crc.is_empty() {
        p.conditions.push("0".into());
    }
    if !fields.agent.is_empty() {
        add(&mut p, "@probe:agent:{}", Value::Text(fields.agent.clone()));
    }
    for word in words.split_whitespace() {
        add(&mut p, r"coalesce(r.display_name, r.name) like {} escape '\'", Value::Text(like_query(word)));
    }
    if let Some(f) = filter {
        let leaves = f.leaves.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        p.conditions.push(format!("@category:{leaves}:{}", f.uncategorized));
    }
    p
}

impl Plan {
    /// This shard's conditions, in SQL.
    fn sql(&self, s: usize, has_poster_index: bool) -> String {
        self.conditions
            .iter()
            .map(|c| {
                if let Some((column, param)) = c.strip_prefix("@probe:").and_then(|rest| rest.split_once(':')) {
                    return format!("r.id in (select release_id from s{s}.release_probe where {column} = {param})");
                }
                if let Some(param) = c.strip_prefix("@email:") {
                    // the whole index is read, which is far less than the table
                    return if has_poster_index {
                        format!(
                            r"r.id in (select id from s{s}.releases indexed by {POSTER_INDEX} where poster like {param} escape '\')"
                        )
                    } else {
                        format!(r"r.poster like {param} escape '\'")
                    };
                }
                c.clone()
            })
            .map(|c| match c.strip_prefix("@category:").and_then(|rest| rest.rsplit_once(':')) {
                Some((leaves, uncategorized)) => {
                    let mut parts = vec![format!(
                        "exists (select 1 from s{s}.release_category c where c.release_id = r.id and c.category in ({leaves}))"
                    )];
                    if uncategorized == "true" {
                        parts.push(format!("not exists (select 1 from s{s}.release_category c where c.release_id = r.id)"));
                    }
                    format!("({})", parts.join(" or "))
                }
                None => c,
            })
            .collect::<Vec<_>>()
            .join(" and ")
    }
}

/// How to build the optional indexes this shard has, for compaction to build them again.
pub fn optional_indexes(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("select sql from sqlite_master where type = 'index' and name in (?1, ?2) and sql is not null")?;
    stmt.query_map([POSTER_INDEX, SIZE_INDEX], |r| r.get(0))?.collect()
}

/// Whether an index can answer this search in this shard.
fn indexed(conn: &Connection, fields: &Fields, shard: usize) -> bool {
    !fields.group.is_empty()
        || !fields.crc.is_empty()
        || !fields.agent.is_empty()
        || ((!fields.poster.is_empty() || !fields.email.is_empty()) && has_index(conn, shard, POSTER_INDEX))
        || (!fields.size.is_empty() && has_index(conn, shard, SIZE_INDEX))
}

fn source(conn: &Connection, fields: &Fields, s: usize) -> String {
    if indexed(conn, fields, s) {
        format!("s{s}.releases r")
    } else {
        format!("(select * from s{s}.releases order by id desc limit {SCAN_LIMIT}) r")
    }
}

fn shards_of(fields: &Fields) -> Vec<usize> {
    if fields.group.is_empty() { (0..store::SHARDS).collect() } else { vec![store::shard_of(&fields.group)] }
}

/// Newest first.
pub fn find(
    fields: &Fields,
    words: &str,
    filter: Option<&CatFilter>,
    offset: i64,
    limit: i64,
) -> Result<Vec<ReleaseRow>> {
    find_with(&db::open()?, fields, words, filter, offset, limit)
}

pub fn find_with(
    conn: &Connection,
    fields: &Fields,
    words: &str,
    filter: Option<&CatFilter>,
    offset: i64,
    limit: i64,
) -> Result<Vec<ReleaseRow>> {
    let plan = plan(fields, words, filter);
    let parts: Vec<String> = shards_of(fields)
        .into_iter()
        .map(|s| {
            format!(
                "select * from (select {COLUMNS} from {} where {} order by r.id desc limit ?1 + ?2)",
                source(conn, fields, s),
                plan.sql(s, has_index(conn, s, POSTER_INDEX))
            )
        })
        .collect();
    let sql = format!("select * from ({}) order by 1 desc limit ?1 offset ?2", parts.join("\nunion all\n"));
    let mut values = vec![Value::Integer(limit), Value::Integer(offset)];
    values.extend(plan.values);
    conn.prepare(&sql)?.query_map(params_from_iter(values.iter()), release_row)?.collect()
}

pub fn count(fields: &Fields, words: &str, filter: Option<&CatFilter>) -> Result<Total> {
    count_with(&db::open()?, fields, words, filter)
}

/// How many match any of these sets of words, each counted once.
pub fn count_any(fields: &Fields, choices: &[String], filter: Option<&CatFilter>) -> Result<Total> {
    let mut plan = plan(fields, "", filter);
    let mut sets = Vec::new();
    for words in choices {
        let mut set = Vec::new();
        for word in words.split_whitespace() {
            plan.values.push(Value::Text(like_query(word)));
            set.push(format!(r"coalesce(r.display_name, r.name) like ?{} escape '\'", plan.values.len() + 2));
        }
        if !set.is_empty() {
            sets.push(format!("({})", set.join(" and ")));
        }
    }
    if sets.len() == choices.len() && !sets.is_empty() {
        plan.conditions.push(format!("({})", sets.join(" or ")));
    }
    count_plan(&db::open()?, fields, &plan)
}

pub fn count_with(conn: &Connection, fields: &Fields, words: &str, filter: Option<&CatFilter>) -> Result<Total> {
    count_plan(conn, fields, &plan(fields, words, filter))
}

fn count_plan(conn: &Connection, fields: &Fields, plan: &Plan) -> Result<Total> {
    let mut total = Total { count: 0, exact: true };
    for s in shards_of(fields) {
        total.exact &= indexed(conn, fields, s);
        let sql = format!(
            "select count(*) from (select 1 from {} where {} limit ?1 offset ?2)",
            source(conn, fields, s),
            plan.sql(s, has_index(conn, s, POSTER_INDEX))
        );
        let mut values = vec![Value::Integer(COUNT_LIMIT), Value::Integer(0)];
        values.extend(plan.values.iter().cloned());
        let n: i64 = conn.query_row(&sql, params_from_iter(values.iter()), |r| r.get(0))?;
        total.exact &= n < COUNT_LIMIT;
        total.count += n;
    }
    Ok(total)
}

/// `atlas --search-indexes [--apply]`: what lets poster, email and size searches see every release.
pub fn run(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let main = crate::naming::walk::main_path();
    if !apply {
        for (s, path) in store::shard_paths(&main).iter().enumerate() {
            let size = std::fs::metadata(path).map_or(0, |m| m.len());
            println!("shard {s}: {path:?}, {size} bytes, would index poster and size");
        }
        println!("add --apply to build them");
        return 0;
    }
    let _lock = match crate::naming::walk::take_lock(&main) {
        Ok(lock) => lock,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    for path in store::shard_paths(&main) {
        let started = std::time::Instant::now();
        let built = db::open_shard(&path).and_then(|conn| {
            conn.execute_batch(&format!(
                "create index if not exists {POSTER_INDEX} on releases(poster);
                 create index if not exists {SIZE_INDEX} on releases(size);"
            ))
        });
        if let Err(e) = built {
            println!("{}: {e}", path.display());
            return 1;
        }
        println!("{}: done in {:?}", path.display(), started.elapsed());
    }
    0
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
    fn a_size_that_is_no_number_finds_nothing() {
        let p = plan(&Fields { size: "abc".into(), ..Fields::default() }, "", None);
        assert!(p.conditions.iter().any(|c| c == "0"));
    }
}
