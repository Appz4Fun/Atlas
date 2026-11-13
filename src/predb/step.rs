//! The step that gives a release its catalogue title.

use crate::config::Config;
use crate::naming::{
    Grade, NameSource, best_from_paths, clean_candidate, current_name, is_image_name, judge, renames_pack_to_episode,
};
use crate::postprocess::step::{
    NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMask, StepMutation,
    StepWrite,
};

use super::keys::{hash_key, name_key};
use super::matcher::Evidence;



const KEYS_KEPT: usize = 4;

pub const PREDB_MATCH: StepId = StepId(16);
/// Parts 7 and 8: ids 24..=39. A match that keeps the same name is not a rename, and they still want to look again.
const LATER_PARTS: StepMask = 0xFFFF_i64 << 24;

/// What no title could equal, without reading anything. A title names a release directory, soo a name with an image
/// extension is out. With one article there is no other file to look at, soo a random name is out too, unless it is a hash.
pub fn worth_looking_up(r: &ReleaseInfo) -> bool {
    if r.name_source.is_some_and(|s| s >= NameSource::PreDb as i64) {
        return false;
    }
    let name = current_name(r);
    if is_image_name(name) {
        return false;
    }
    if r.parts > 1 {
        return true;
    }
    if clean_candidate(name).as_deref().and_then(hash_key).is_some() {
        return true;
    }
    judge(name) != Grade::Junk
}

pub struct PredbMatch;

impl Step for PredbMatch {
    fn id(&self) -> StepId {
        PREDB_MATCH
    }
    fn name(&self) -> &'static str {
        "predb_match"
    }
    fn rank(&self) -> u16 {
        300
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.predb.enabled
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        worth_looking_up(r)
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let inner = ctx.data().archive_inner_files(id)?;

        let r = ctx.release();
        let evidence = Evidence {
            name: &r.name,
            display_name: r.display_name.as_deref(),
            size: r.size,
            files: files.iter().map(|f| f.filename.as_str()).collect(),
            inner: inner.iter().map(|f| f.name.as_str()).collect(),
        };
        let lookup = match ctx.data().predb_find(&evidence)? {
            crate::pg::predb_cache::LookupStatus::Pending => {
                return Err(StepError::Transient("PreDB matcher index is rebuilding".into()));
            }
            crate::pg::predb_cache::LookupStatus::Ready(lookup) => lookup,
        };
        let found = lookup.found;
        let revision = lookup.revision;
        let guard = || StepWrite::for_mutation(id, StepMutation::PredbRead { revision });

        let hit = found.filter(|m| !renames_pack_to_episode(current_name(r), &m.title.title));
        let lookup = if hit.is_some() { "matched" } else { "no_match" };
        crate::metrics::names::PREDB_LOOKUPS.inc(&[("result", lookup)], 1);
        let Some(m) = hit else {
            let mut writes: Vec<_> = vec![guard()];
            if !inner.is_empty() {
                writes.push(StepWrite::for_mutation(id, StepMutation::ClearPredbInnerKeys));
                let paths = inner.iter().map(|f| (f.name.as_str(), f.size.unwrap_or(1).max(1) as u64));
                for name in best_from_paths(paths, Grade::Weak).into_iter().take(KEYS_KEPT) {
                    writes.push(StepWrite::for_mutation(id, StepMutation::AddPredbInnerKey { key: name_key(&name) }));
                }
            }
            return Ok(Outcome { writes, ..Outcome::default() });
        };

        let t = m.title;
        let how = match m.kind {
            super::matcher::Kind::Exact => "exact",
            super::matcher::Kind::Normalized => "normalized",
            super::matcher::Kind::Hash => "hash",
            super::matcher::Kind::InnerFile => "inner_file",
        };
        let predb_category = crate::metrics::predb_category_label(t.category.as_deref().unwrap_or("none"));
        crate::metrics::names::PREDB_MATCHES.inc(&[("kind", how), ("category", &predb_category)], 1);
        let known = ctx.data().prior_predb_match(id)?;
        let now = chrono::Utc::now().timestamp();
        let mut facts = vec![("predb.title", t.title.clone())];
        if let Some(category) = &t.category {
            facts.push(("predb.category", category.clone()));
        }
        let mut writes: Vec<_> = vec![guard()];
        writes.extend([
            StepWrite::for_mutation(
                id,
                StepMutation::KeepPredbMatch {
                    title_id: t.id,
                    kind: m.kind,
                    category: t.category.clone(),
                    pre_at: t.pre_at,
                    nuked: t.nuked,
                    matched_at: now,
                },
            ),
            StepWrite::for_mutation(id, StepMutation::ClearPredbInnerKeys),
        ]);
        Ok(Outcome {
            matched: true,
            writes,
            // a title outranks every other name, soo this goes around naming::improves
            name: Some(NameChange {
                display_name: Some(t.title.clone()),
                obfuscated: Some(false),
                source: NameSource::PreDb.value(),
            }),
            rerun: if known == Some(t.id) { 0 } else { LATER_PARTS },
            facts,
            wait_until: None,
            junk: None,
            ..Outcome::default()
        })
    }
}
