//! A release's NFO, read once and kept: the probe keeps the one it read in its own JSON (`nfo`, `nfo_file`), and a
//! step that read one before the probe did (the archive listing looking for a password, the name or IMDb step on a
//! release the probe passed over) keeps it here. Every reader looks here first, soo an NFO's article is fetched at
//! most once a release. Only an NFO read from its file's first article is kept: a later article's text, read while
//! the first wasn't indexed yet, isn't the NFO, and the first is read once it's there.

use crate::postprocess::step::{StepMutation, StepWrite};

/// As much of an NFO as is kept, in characters: the probe's own cap.
pub const MAX_CHARS: usize = 256 * 1024;

/// An NFO already read for a release.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredNfo {
    /// the release file it was read from
    pub file: String,
    pub text: String,
}

impl StoredNfo {
    /// Whether this is the NFO in `file`, posted under that name or with `par2_name` as its PAR2 set names it.
    pub fn is_of(&self, file: &str, par2_name: Option<&str>) -> bool {
        self.file == file || par2_name == Some(self.file.as_str())
    }
}

/// Keeps an NFO a step read, for the probe and the other steps to use in place of fetching it again.
pub fn keep(release_id: i64, file: &str, text: &str) -> StepWrite {
    let text: String = text.chars().take(MAX_CHARS).collect();
    StepWrite::for_mutation(release_id, StepMutation::KeepNfo { file: file.to_string(), text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_nfo_matches_only_its_file_or_par2_name() {
        let named = StoredNfo { file: "Real.nfo".into(), text: String::new() };
        assert!(named.is_of("Real.nfo", None));
        assert!(named.is_of("x7f3a9", Some("Real.nfo")));
        assert!(!named.is_of("Other.nfo", None));
        assert!(!named.is_of("real.nfo", None));
    }

    #[test]
    fn it_is_capped_by_characters_without_splitting_unicode() {
        let write = keep(8, "a.nfo", &"é".repeat(MAX_CHARS + 10));
        let StepMutation::KeepNfo { file, text } = write.mutation() else { panic!("typed NFO mutation") };
        assert_eq!(file, "a.nfo");
        assert_eq!(text.chars().count(), MAX_CHARS);
        assert_eq!(text.len(), MAX_CHARS * 2);
    }

    #[test]
    #[ignore = "requires isolated PostgreSQL test database"]
    fn native_probe_nfo_precedes_first_kept_nfo_and_partial_probe_cannot_replace_it() {
        use crate::pg::{
            stage_queue::{Queue, Settlement, Visit},
            stage_reads::StepReads,
        };
        use crate::postprocess::apply::Applied;
        let schema = crate::pg::testing::Schema::new().expect("PostgreSQL fixture required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let queue = Queue::for_owner(&runtime.owner).unwrap();
        let claims = queue.claim(10, 1, 60).unwrap();
        assert_eq!(claims.len(), 1);
        let claim = &claims[0];
        let id = claim.release_id();
        let reads = StepReads::for_owner(&runtime.owner).unwrap();
        assert_eq!(reads.stored_nfo(id).unwrap(), None);
        let applied = Applied {
            release_id: id,
            set_done: 4,
            writes: vec![(4, keep(id, "a.nfo", "kept by a step")), (4, keep(id, "b.nfo", "another"))],
            ..Default::default()
        };
        assert_eq!(
            queue.settle(claim, Visit::prepare(claim, applied, [9; 32]).unwrap(), 11).unwrap(),
            Settlement::Applied
        );
        assert_eq!(
            reads.stored_nfo(id).unwrap(),
            Some(StoredNfo { file: "a.nfo".into(), text: "kept by a step".into() })
        );
        let set_probe = |probe: crate::probe::ProbeInfo| {
            let json = serde_json::to_string(&probe).unwrap();
            runtime.owner.with(|client| {
                client.execute("insert into release_probe(release_id,info) values($1,$2) on conflict(release_id) do update set info=excluded.info", &[&id,&json])?;
                Ok(())
            }).unwrap();
        };
        set_probe(crate::probe::ProbeInfo {
            nfo: Some("from the probe".into()),
            nfo_file: Some("probe.nfo".into()),
            ..Default::default()
        });
        assert_eq!(
            reads.stored_nfo(id).unwrap(),
            Some(StoredNfo { file: "probe.nfo".into(), text: "from the probe".into() })
        );
        set_probe(crate::probe::ProbeInfo { nfo: Some("part 2".into()), ..Default::default() });
        assert_eq!(reads.stored_nfo(id).unwrap().unwrap().text, "kept by a step");
        set_probe(Default::default());
        assert_eq!(reads.stored_nfo(id).unwrap().unwrap().text, "kept by a step");
        runtime
            .owner
            .with(|client| {
                assert_eq!(client.query_one("select count(*) from native_operations", &[])?.get::<_, i64>(0), 1);
                client.execute("delete from release_nfo where release_id=$1", &[&id])?;
                Ok(())
            })
            .unwrap();
        set_probe(crate::probe::ProbeInfo { nfo: Some("part 2".into()), ..Default::default() });
        assert_eq!(reads.stored_nfo(id).unwrap(), None);
        runtime.owner.close();
    }
}
