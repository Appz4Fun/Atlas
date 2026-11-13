//! The step that gives a release its category.

use std::sync::RwLock;

#[cfg(test)]
use rusqlite::Connection;

use crate::config::Config;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite};

use super::rules::{Options, classify_release, stores};

pub const RELEASE_CATEGORY: ShardTable = ShardTable {
    name: "release_category",
    create: "create table if not exists release_category (release_id INTEGER PRIMARY KEY, category INTEGER NOT NULL)",
    indexes: &[(
        "category_release",
        "create index if not exists category_release on release_category(category, release_id)",
    )],
    release_key: Some("release_id"),
    added: &[],
};

/// The kind and subcategories of release `id`'s spot, when it was saved from one (`spotnet::store::SPOTS`).
#[cfg(test)]
fn spot_of(conn: &Connection, id: i64) -> Result<Option<(crate::spotnet::Kind, crate::spotnet::Subcats)>, StepError> {
    crate::postprocess::data::StepData::LegacySqlite(conn).category_spot(id)
}

/// The category step's id, for a rerun of it alone.
pub const STEP: StepId = StepId(24);

pub struct CategoryStep {
    options: RwLock<Options>,
}

impl CategoryStep {
    pub fn new() -> CategoryStep {
        CategoryStep { options: RwLock::new(Options::default()) }
    }
}

impl Default for CategoryStep {
    fn default() -> Self {
        CategoryStep::new()
    }
}

impl Step for CategoryStep {
    fn id(&self) -> StepId {
        STEP
    }
    fn name(&self) -> &'static str {
        "category"
    }
    fn rank(&self) -> u16 {
        400
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.options.write().unwrap() = cfg.category.options();
        cfg.category.enabled
    }
    // `parts` growing means a save may have set a name without a NameChange
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let mut d = classify_release(ctx.release(), &self.options.read().unwrap());
        // a release saved from a spot: the spotter said movie or series, which wins over a pick elsewhere
        if let Some((kind, subcats)) = ctx.data().category_spot(id)? {
            let release = ctx.release();
            let name = release.display_name.as_deref().unwrap_or(&release.name);
            let picked = if stores(&d) { d.category } else { 0 };
            if let Some(category) = crate::spotnet::category::hint(kind, &subcats, name, picked) {
                d = super::Decision { category, confidence: 900, rule: "spot_kind" };
            }
        }
        // there is no PC category: a PC match is junk, unless it has video, which then goes uncategorized
        if crate::junk::is_pc(d.category) {
            let files = ctx.files().map_err(StepError::from_inventory)?;
            if crate::junk::pc_is_junk(files.iter().map(|f| f.filename.as_str())) {
                return Ok(Outcome { junk: Some(crate::junk::PC), ..Outcome::default() });
            }
            d = super::Decision { category: super::UNCATEGORIZED, confidence: 0, rule: "no_match" };
        }
        if stores(&d) {
            let category = super::label(d.category).unwrap_or_else(|| d.category.to_string());
            let root = super::label(super::root_of(d.category)).unwrap_or_else(|| "Other".into());
            let names = [("category", category.as_str()), ("root", root.as_str())];
            crate::metrics::names::CATEGORIZED.inc(&names, 1);
            crate::metrics::names::RELEASE_SIZE.observe(&[("root", &root)], ctx.release().size.max(0) as f64);
        }
        let writes = if stores(&d) {
            vec![StepWrite::for_mutation(id, StepMutation::SetCategory { category: d.category })]
        } else if ctx.data().category_exists(id)? {
            vec![StepWrite::for_mutation(id, StepMutation::ClearCategory)]
        } else {
            Vec::new()
        };
        Ok(Outcome {
            matched: stores(&d),
            writes,
            facts: vec![("category", d.category.to_string()), ("category_rule", d.rule.to_string())],
            ..Outcome::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_releases_spot() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::spotnet::store::SPOTS.create).unwrap();
        conn.execute(
            "insert into spots (release_id, message_id, kind, title, subcats) values (8, 'a', 'series', 'A', 'a9|d18|z1|'), (16, 'b', 'book', 'B', '')",
            [],
        )
        .unwrap();
        let (kind, subcats) = spot_of(&conn, 8).unwrap().unwrap();
        assert_eq!(kind, crate::spotnet::Kind::Series);
        assert_eq!(subcats.d, [18]);
        assert!(spot_of(&conn, 16).unwrap().is_none(), "not a kind Atlas takes");
        assert!(spot_of(&conn, 24).unwrap().is_none());
    }
}
