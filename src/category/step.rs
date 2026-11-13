//! The step that gives a release its category.

use std::sync::RwLock;

use crate::config::Config;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite};

use super::rules::{Options, classify_release, stores};

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

    // Port of the former spot_of SQLite fixture through the genuine original-XML producer/owner.
    #[test]
    #[ignore = "explicit isolated PG; original Spotnet kind and subcategories through StepReads"]
    fn reads_a_releases_spot() {
        use crate::postprocess::data::StepData;
        let (_schema, runtime, _directory, id) = crate::spotnet::native_read_fixture_tests::fixture();
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let data = StepData::Native(&reads);
        let original = crate::spotnet::full::parse(crate::spotnet::native_read_fixture_tests::XML).unwrap();
        let (kind, subcats) = data.category_spot(id).unwrap().unwrap();
        assert_eq!(kind, crate::spotnet::Kind::Series);
        assert_eq!(subcats, original.subcats);
        assert_eq!(subcats.z, [1]);
        assert_eq!(reads.spot_detail(id).unwrap().unwrap().spot, original, "the original XML is unchanged");
        let no_spot = runtime
            .owner
            .with(|client| {
                crate::pg::release_writer::save(
                    client,
                    &[crate::parser::Release {
                        name: "Ordinary.Release".into(),
                        group: "example.no.spot".into(),
                        articles: vec![crate::parser::Article {
                            filename: Some("sample.bin".into()),
                            message_id: "<category-no-spot@fixture.test>".into(),
                            part: Some(1),
                            total_parts: Some(1),
                            bytes: 1,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    -100_000,
                )?;
                Ok(client.query_one("select id from releases where group_name='example.no.spot'", &[])?.get(0))
            })
            .unwrap();
        assert!(data.category_spot(no_spot).unwrap().is_none());
        runtime
            .owner
            .with(|client| {
                client.execute("update spots set kind='book' where release_id=$1", &[&id])?;
                Ok(())
            })
            .unwrap();
        // Unsupported stored metadata is corruption under the native original-byte contract, not a miss.
        let error = data.category_spot(id).unwrap_err();
        assert!(
            matches!(error, StepError::Native(ref cause) if cause.to_string().contains("native Spotnet kind does not match its original"))
        );
        runtime.owner.close();
        assert!(matches!(data.category_spot(no_spot), Err(StepError::Native(_))));
    }
}
