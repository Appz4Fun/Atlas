//! The step that gives a release its category.

use std::sync::RwLock;

use rusqlite::{Connection, OptionalExtension};

use crate::config::Config;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepWrite};

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

const UPSERT: &str = "insert or replace into release_category (release_id, category) values (?1, ?2)";
const DELETE: &str = "delete from release_category where release_id = ?1";

fn has_row(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let found: Option<i64> = conn
        .prepare_cached("select 1 from release_category where release_id = ?1")?
        .query_row([id], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
}

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
        StepId(24)
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
        let d = classify_release(ctx.release(), &self.options.read().unwrap());
        if stores(&d) {
            let category = super::label(d.category).unwrap_or_else(|| d.category.to_string());
            let root = super::label(super::root_of(d.category)).unwrap_or_else(|| "Other".into());
            let names = [("category", category.as_str()), ("root", root.as_str())];
            crate::metrics::names::CATEGORIZED.inc(&names, 1);
            crate::metrics::names::RELEASE_SIZE.observe(&[("root", &root)], ctx.release().size.max(0) as f64);
        }
        let writes = if stores(&d) {
            vec![StepWrite::new(UPSERT, vec![id.into(), d.category.into()])]
        } else if has_row(ctx.conn(), id).map_err(|e| StepError::Retry(e.to_string()))? {
            vec![StepWrite::new(DELETE, vec![id.into()])]
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
