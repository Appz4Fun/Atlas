//! The Spotnet blacklist: spotters known for spam, fakes and the like, as Spotweb takes them from `blacklist_url`
//! (an XML list of the spotters' public keys, `<Keys><Key Name="...">MODULUS</Key>...</Keys>`). Kept in the main
//! database's `spot_blacklist` by spotter id, read again once a day. A blacklisted spotter's spots aren't fetched,
//! their releases lose the spot tag, and their comments are hidden (`store::disposed_sql`).

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, params};

/// Where Spotweb reads the blacklist.
pub const DEFAULT_URL: &str = "https://spotlist.store/spotnet/blacklist.xml";
/// How often the list is read again.
pub const REFRESH_SECS: i64 = 86_400;
/// The largest list read: the real one is under 100 KB.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// When the list was last read, in the main database's `meta`.
const READ_AT: &str = "spot_blacklist_at";

/// The main database's table, if missing.
pub fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("create table if not exists spot_blacklist (spotter TEXT PRIMARY KEY) without rowid")
}

/// The spotter ids a blacklist names, each once. Spotweb takes ids of 3 to 6 characters, and so does this.
pub fn parse(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<Key") {
        rest = &rest[open + 4..];
        // `<Keys>` is the list, not a key
        if !rest.starts_with(['>', ' ', '\t', '\r', '\n']) {
            continue;
        }
        let Some(start) = rest.find('>') else { break };
        let Some(end) = rest[start..].find("</Key>") else { break };
        let key = rest[start + 1..start + end].trim();
        if let Some(id) = super::header::spotter_id(key).filter(|id| (3..=6).contains(&id.len()))
            && !out.contains(&id)
        {
            out.push(id);
        }
        rest = &rest[start + end..];
    }
    out
}

/// Reads the list at `url`. Blocking: call it off the runtime's threads.
pub fn fetch(url: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| anyhow!("{url}: {e}"))?;
    Ok(response.body_mut().with_config().limit(MAX_BYTES).read_to_string()?)
}

/// Whether the list is due to be read again at `now`.
pub fn due(conn: &Connection, now: i64) -> rusqlite::Result<bool> {
    let at = super::store::read_cursor(conn, READ_AT)?.and_then(|at| i64::try_from(at).ok());
    Ok(at.is_none_or(|at| now - at >= REFRESH_SECS))
}

/// Puts `spotters` in place of the list kept, and notes when. An empty list is no list (a page that changed or an
/// error page): the one kept stays, and it's read again at the next refresh. Returns how many are listed.
pub fn replace(conn: &mut Connection, spotters: &[String], now: i64) -> Result<usize> {
    if spotters.is_empty() {
        bail!("the blacklist names no spotters");
    }
    let tx = conn.transaction()?;
    tx.execute("delete from spot_blacklist", [])?;
    {
        let mut insert = tx.prepare_cached("insert or ignore into spot_blacklist (spotter) values (?)")?;
        for s in spotters {
            insert.execute(params![s])?;
        }
    }
    super::store::write_cursor(&tx, READ_AT, now.max(0) as u64)?;
    tx.commit()?;
    Ok(spotters.len())
}

/// Notes a failed read, so it's tried again at the next refresh, not every pass.
pub fn tried(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    super::store::write_cursor(conn, READ_AT, now.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // two entries of the real list (spotlist.store, November 2026), one key twice, a blank and a non-key
    const LIST: &str = r#"<Keys>
  <Key Name="12garfield56">rKflaUf7aBQ6pFGm34N/sf1klvSsk9RmCLQClebgdFfo8lXr1l7NKuty77wo4qsn</Key>
  <Key Name="aaaaaa">1COQWpNScg8ugJwHAvRVD9V2+HlaMTv2yKhdrKRJgZ2DirCXT/EmripwUvXMqMlz</Key>
  <Key Name="again">rKflaUf7aBQ6pFGm34N/sf1klvSsk9RmCLQClebgdFfo8lXr1l7NKuty77wo4qsn</Key>
  <Key Name="blank"></Key>
  <KeyNote>not a key</KeyNote>
</Keys>"#;

    #[test]
    fn reads_the_spotter_ids() {
        let ids = parse(LIST);
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(
            ids[0],
            crate::spotnet::header::spotter_id("rKflaUf7aBQ6pFGm34N/sf1klvSsk9RmCLQClebgdFfo8lXr1l7NKuty77wo4qsn")
                .unwrap()
        );
        assert!(parse("<html>not found</html>").is_empty());
    }

    #[test]
    fn replaces_the_list_and_keeps_it_on_an_empty_one() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table meta (key TEXT PRIMARY KEY, value INTEGER)").unwrap();
        create(&conn).unwrap();
        assert!(due(&conn, 100).unwrap());
        assert_eq!(replace(&mut conn, &["abc".into(), "def".into()], 100).unwrap(), 2);
        assert!(!due(&conn, 100 + REFRESH_SECS - 1).unwrap());
        assert!(due(&conn, 100 + REFRESH_SECS).unwrap());
        assert!(replace(&mut conn, &[], 200).is_err());
        assert_eq!(replace(&mut conn, &["xyz".into()], 300).unwrap(), 1);
        let left: Vec<String> = conn
            .prepare("select spotter from spot_blacklist")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(left, ["xyz"]);
        tried(&conn, 400).unwrap();
        assert!(!due(&conn, 401).unwrap());
    }
}
