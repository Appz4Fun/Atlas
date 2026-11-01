//! Real Newznab categories, in place of every release being 7000.

pub mod lists;
pub mod rules;
pub mod step;
pub mod table;

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use crate::{db, store};

pub use rules::{Decision, Input, Options, classify, classify_release, stores};
pub use step::{CategoryStep, RELEASE_CATEGORY};
pub use table::{
    CatFilter, HASHED, MISC, TABLE, UNCATEGORIZED, caps_xml, is_movie, is_tv, label, parse_filter, root_of,
};

/// Per shard primary key reads. A release with no row is not in the map.
pub fn categories_of(ids: &[i64]) -> db::Result<HashMap<i64, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    categories_of_with(&db::open()?, ids)
}

pub fn categories_of_with(conn: &Connection, ids: &[i64]) -> db::Result<HashMap<i64, i64>> {
    let mut by_shard: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for id in ids {
        by_shard.entry(store::shard_of_id(*id)).or_default().push(id.to_string());
    }
    let mut out = HashMap::with_capacity(ids.len());
    for (shard, list) in by_shard {
        let sql = format!(
            "select release_id, category from s{shard}.release_category where release_id in ({})",
            list.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, category) = row?;
            out.insert(id, category);
        }
    }
    Ok(out)
}
