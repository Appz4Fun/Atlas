//! Checked Step reads. Native admission waits for a reviewed resolver and epoch protocol.

use rusqlite::{Connection, OptionalExtension};

use super::step::StepError;

/// Read data is separate from mutation authority. No variant manufactures a native release reference.
pub enum StepData<'a> {
    LegacySqlite(&'a Connection),
    /// There is no accepted native Stage resolver or snapshot issuer yet.
    NativeUnavailable,
}

impl StepData<'_> {
    fn legacy(&self) -> Result<&Connection, StepError> {
        match self {
            Self::LegacySqlite(conn) => Ok(conn),
            Self::NativeUnavailable => Err(StepError::Retry("native Step data is unavailable".into())),
        }
    }

    pub(crate) fn legacy_connection(&self) -> Result<&Connection, StepError> {
        self.legacy()
    }

    fn legacy_inventory_connection(&self) -> rusqlite::Result<&Connection> {
        match self {
            Self::LegacySqlite(conn) => Ok(conn),
            Self::NativeUnavailable => Err(rusqlite::Error::UserFunctionError(Box::new(std::io::Error::new(
                std::io::ErrorKind::Unsupported,"native Step inventory is unavailable",
            )))),
        }
    }

    /// Compatibility for legacy Steps' existing error handling. Unavailable native data is always an error.
    pub fn release_files(&self, release_id: i64) -> rusqlite::Result<Vec<crate::store::FileInfo>> {
        crate::store::release_files(self.legacy_inventory_connection()?, release_id, 5_000)
    }

    pub fn file_segments(&self, file_id: i64, pick: &crate::store::SegmentPick) -> rusqlite::Result<Vec<crate::store::SegmentRef>> {
        crate::store::file_segments(self.legacy_inventory_connection()?, file_id, pick)
    }

    pub fn probe_moviehash(&self, release_id: i64) -> Result<Option<String>, StepError> {
        self.legacy()?
            .query_row("select moviehash from release_probe where release_id = ?1", [release_id], |row| row.get(0))
            .optional()
            .map(Option::flatten)
            .map_err(|_| StepError::Retry("OpenSubtitles probe hash read failed".into()))
    }

    pub fn opensubtitles_asked(&self, release_id: i64, hash: &str) -> Result<bool, StepError> {
        self.legacy()?
            .query_row(
                "select 1 from release_opensubtitles where release_id = ?1 and moviehash = ?2",
                rusqlite::params![release_id, hash],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(|_| StepError::Retry("OpenSubtitles prior match read failed".into()))
    }

    pub fn opensubtitles_movie_imdb(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        self.legacy()?
            .query_row(
                "select imdb_id from release_opensubtitles
                 where release_id = ?1 and imdb_id is not null and lower(kind) = 'movie'",
                [release_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StepError::Retry("OpenSubtitles movie IMDb read failed".into()))
    }

    pub fn category_exists(&self, release_id: i64) -> Result<bool, StepError> {
        self.legacy()?.query_row("select 1 from release_category where release_id=?1", [release_id], |_| Ok(()))
            .optional().map(|row| row.is_some()).map_err(|_| StepError::Retry("category read failed".into()))
    }

    pub fn category_spot(&self, release_id: i64) -> Result<Option<(crate::spotnet::Kind, crate::spotnet::Subcats)>, StepError> {
        let row: Option<(String, String)> = self.legacy()?
            .query_row("select kind,coalesce(subcats,'') from spots where release_id=?1", [release_id],
                |row| Ok((row.get(0)?, row.get(1)?)))
            .optional().map_err(|_| StepError::Retry("category spot read failed".into()))?;
        Ok(row.and_then(|(kind, subcats)| {
            let kind = match kind.as_str() {
                "movie" => crate::spotnet::Kind::Movie,
                "series" => crate::spotnet::Kind::Series,
                _ => return None,
            };
            Some((kind, crate::spotnet::header::subcats(&subcats)))
        }))
    }

    pub fn probe_info(&self, release_id: i64) -> Result<Option<String>, StepError> {
        self.legacy()?.query_row("select info from release_probe where release_id=?1", [release_id], |row| row.get(0))
            .optional().map_err(|_| StepError::Retry("probe information read failed".into()))
    }

    /// Retains the legacy best-effort rerun marker policy; unavailable native data still returns an error.
    pub fn explicit_rerun(&self, release_id: i64, step: super::step::StepId) -> Result<bool, StepError> {
        Ok(crate::raw_store::is_explicit_rerun(self.legacy()?, release_id, step))
    }

    /// A legacy accounting read failure defers eviction as before; native unavailability is never a zero.
    pub fn sidecar_text_bytes(&self) -> Result<i64, StepError> {
        Ok(crate::sidecar::text_bytes(self.legacy()?))
    }

    pub fn archive_inner_files(&self, release_id: i64) -> Result<Vec<crate::archive::store::InnerFileRow>, StepError> {
        crate::archive::store::inner_files(self.legacy()?, release_id).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn archive_password(&self, release_id: i64) -> Result<Option<crate::archive::Password>, StepError> {
        crate::archive::store::password(self.legacy()?, release_id).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn rename_named(&self, group: &str) -> Result<bool, StepError> {
        self.legacy()?.query_row("select value from meta where key in ('naming_rename_named',?1) limit 1",
            [crate::postprocess::reprocess::rename_named_key(group)], |row| row.get::<_, i64>(0))
            .optional().map(|row| row.is_some()).map_err(|error| StepError::Retry(error.to_string()))
    }

    /// Legacy content identity also installs its existing sibling raw-store path binding.
    pub fn release_content_id(&self, release_id: i64) -> Result<Option<Vec<u8>>, StepError> {
        Ok(crate::raw_store::release_content_id(self.legacy()?, release_id))
    }

    pub fn nfo_password(&self, release_id: i64) -> Result<Option<String>, StepError> {
        Ok(crate::archive::store::nfo_password(self.legacy()?, release_id))
    }

    pub fn local_password_check(&self, release_id: i64) -> Result<Option<crate::archive::PasswordTest>, StepError> {
        Ok(crate::archive::store::local_password_check(self.legacy()?, release_id))
    }

    pub fn stored_nfo(&self, release_id: i64) -> Result<Option<crate::probe::nfo_store::StoredNfo>, StepError> {
        Ok(crate::probe::nfo_store::stored(self.legacy()?, release_id))
    }

    pub fn par2_names(&self, release_id: i64, files: &[crate::store::FileInfo]) -> Result<std::collections::HashMap<String,String>, StepError> {
        Ok(crate::archive::select::par2_name_map(self.legacy()?, release_id, files))
    }

    /// These legacy hints are best effort. Native data cannot fall through to that policy.
    pub fn archive_requires_password_list(&self, release_id: i64) -> Result<bool, StepError> {
        let conn = self.legacy()?;
        let names_hidden = conn.query_row(
            "select 1 from archive_release ar where ar.release_id=?1 and (
                (ar.names_hidden=1 and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in (1,2,3)))
                or (ar.password=1 and exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source in (1,2,3) and lower(f.name) like '%.nzb')
                    and not exists(select 1 from archive_inner_files f where f.release_id=ar.release_id and f.source=5)))",
            [release_id], |_| Ok(true)).unwrap_or(false);
        Ok(names_hidden)
    }

    pub fn nfo_password_rerun_flags(&self, release_id: i64) -> Result<(bool,bool), StepError> {
        let names_hidden = self.archive_requires_password_list(release_id)?;
        let conn = self.legacy()?;
        let probe_encrypted = conn.query_row(
            "select 1 from release_probe where release_id=?1 and info like '%\"encrypted\":true%'",
            [release_id], |_| Ok(true)).unwrap_or(false);
        Ok((names_hidden, probe_encrypted))
    }

    pub fn media_imdb_marker(&self, release_id: i64) -> Result<Option<Option<i64>>, StepError> {
        self.legacy()?.query_row("select imdb_id from media_release where release_id=?1", [release_id], |row| row.get(0))
            .optional().map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn media_exists(&self, release_id: i64) -> Result<bool, StepError> {
        self.legacy()?.query_row("select 1 from media_release where release_id=?1", [release_id], |_| Ok(()))
            .optional().map(|row| row.is_some()).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn saved_probe_media(&self, release_id: i64) -> Result<Option<crate::probe::media::MediaInfo>, StepError> {
        Ok(crate::media::steps::saved_media(self.legacy()?, release_id))
    }

    pub fn stale_media_ids(&self, title_key: i64) -> Result<bool, StepError> {
        crate::media::steps::stale_ids(self.legacy()?, title_key).map_err(|error| StepError::Retry(error.to_string()))
    }

    /// Explicit legacy catalog dependency. This does not open a reader or choose a native replacement.
    pub fn predb_catalog_path(&self) -> Result<std::path::PathBuf, StepError> {
        self.legacy()?.path().and_then(|path| crate::predb::step::db_path_from_shard(std::path::Path::new(path)))
            .ok_or_else(|| StepError::Retry("this shard's PreDB file cant be worked out".into()))
    }

    pub fn prior_predb_match(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        self.legacy()?.query_row("select title_id from predb_match where release_id=?1", [release_id], |row| row.get(0))
            .optional().map_err(|error| StepError::Retry(error.to_string()))
    }

    /// Legacy salt creation stays explicit; native data cannot reuse a cached legacy salt.
    pub fn key_salt(&self, cached: Option<i64>) -> Result<i64, StepError> {
        crate::keys::legacy_step_salt(self.legacy()?, cached)
    }

    pub fn release_keys(&self, release_id: i64, parts: Option<i64>, salt: i64) -> Result<crate::keys::Keys, StepError> {
        crate::keys::read_keys(self.legacy()?, release_id, parts, Some(salt)).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn stored_release_keys(&self, release_id: i64) -> Result<Option<crate::keys::Keys>, StepError> {
        crate::keys::stored(self.legacy()?, release_id).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn probe_info_best_effort(&self, release_id: i64) -> Result<Option<String>, StepError> {
        Ok(self.legacy()?.query_row("select info from release_probe where release_id=?1", [release_id], |row| row.get(0)).ok())
    }

    pub fn kept_stills(&self, release_id: i64) -> Result<Vec<(String,bool)>, StepError> {
        self.legacy()?.prepare("select sha256,phash is not null from release_stills where release_id=?1")
            .and_then(|mut query| query.query_map([release_id], |row| Ok((row.get(0)?,row.get(1)?)))?.collect())
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn has_audio_print(&self, release_id: i64) -> Result<bool, StepError> {
        Ok(crate::probe::audio_print::has_print(self.legacy()?, release_id))
    }

    /// Explicit legacy cross-shard dependency; no native context can obtain a SQLite path here.
    pub fn legacy_main_path(&self) -> Result<std::path::PathBuf, StepError> {
        self.legacy()?.path().and_then(crate::probe::audio_print::main_of)
            .ok_or_else(|| StepError::Retry("no main database".into()))
    }

    pub fn release_lookup(&self, release_id: i64, parts: Option<i64>) -> Result<crate::keys::Lookup, StepError> {
        crate::keys::read_lookup(self.legacy()?, release_id, parts, None).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn stills_lookup(&self, release_id: i64) -> Result<crate::keys::Lookup, StepError> {
        Ok(crate::probe::stills::file_lookup(self.legacy()?, "main", release_id))
    }

    pub fn par2_file_names(&self, release_id: i64) -> Result<Vec<String>, StepError> {
        Ok(crate::archive::select::par2_file_names(self.legacy()?, release_id))
    }

    pub(crate) fn same_mux_media(&self, release_id: i64, info: &crate::probe::ProbeInfo, stamp: u64) -> Result<Option<crate::probe::step::SameMux>, StepError> {
        Ok(crate::probe::step::same_mux_of_legacy(self.legacy()?, release_id, info, stamp))
    }

    pub fn linked_image_files(&self, release_id: i64) -> Result<Vec<crate::store::FileInfo>, StepError> {
        crate::images::step::linked_files(self.legacy()?, release_id).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn requested_image_file(&self, release_id: i64, name: &str) -> Result<Option<crate::store::FileInfo>, StepError> {
        crate::images::step::asked_linked_file(self.legacy()?, release_id, name).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn image_requests(&self, release_id: i64) -> Result<Vec<String>, StepError> {
        self.legacy()?.prepare_cached("select file from picture_requests where release_id=?1 and (done_at is null or outcome='shown')")
            .and_then(|mut query| query.query_map([release_id], |row| row.get(0))?.collect())
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn kept_images(&self, release_id: i64) -> Result<std::collections::HashMap<String,String>, StepError> {
        crate::images::step::kept_before(self.legacy()?, release_id).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn archive_source_name_bytes(&self, release_id: i64, source: i64) -> Result<i64, StepError> {
        self.legacy()?.query_row(crate::archive::store::BYTES_OF_SOURCE, rusqlite::params![release_id,source], |row| row.get(0))
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn archive_total_name_bytes(&self) -> Result<i64, StepError> {
        crate::store::get_meta(self.legacy()?, "archive_name_bytes").map(|value| value.unwrap_or(0))
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn legacy_step_key_salt(&self) -> Result<Option<i64>, StepError> {
        Ok(crate::keys::step_salt(self.legacy()?))
    }

    pub fn upload_lead(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        self.legacy()?.query_row("select lead_id from upload_link where release_id=?1", [release_id], |row| row.get(0))
            .optional().map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn upload_tried_count(&self, release_id: i64) -> Result<i64, StepError> {
        self.legacy()?.query_row("select count(*) from upload_link_tried where release_id=?1", [release_id], |row| row.get(0))
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn release_size(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        self.legacy()?.query_row("select coalesce(size,0) from releases where id=?1", [release_id], |row| row.get(0))
            .optional().map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn upload_is_lead(&self, release_id: i64) -> Result<bool, StepError> {
        self.legacy()?.query_row("select 1 from upload_link where release_id=?1 and lead_id=?1", [release_id], |_| Ok(()))
            .optional().map(|row| row.is_some()).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn release_name_marker(&self, release_id: i64) -> Result<Option<Option<String>>, StepError> {
        Ok(self.legacy()?.query_row("select display_name from releases where id=?1", [release_id], |row| row.get(0)).optional().unwrap_or(None))
    }

    pub fn posted_age_days(&self, posted: &str) -> Result<Option<f64>, StepError> {
        Ok(self.legacy()?.query_row("select julianday('now')-julianday(?)", [posted], |row| row.get(0)).ok().flatten())
    }

    pub fn file_clock(&self, file_id: i64) -> Result<Option<crate::archive::link::Clock>, StepError> {
        Ok(self.legacy()?.query_row("select first_date,last_date,clock_first,clock_last from file_timing where file_id=?1", [file_id],
            |row| Ok(crate::archive::link::Clock::of(row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).ok().flatten())
    }

    pub fn par2_inventory(&self, release_id: i64) -> Result<Option<crate::par2::Inventory>, StepError> {
        Ok(crate::archive::store::par2_inventory(self.legacy()?, "main", release_id))
    }

    pub fn archive_source_exists(&self, release_id: i64, source: i64) -> Result<bool, StepError> {
        Ok(self.legacy()?.query_row("select exists(select 1 from archive_inner_files where release_id=?1 and source=?2)",
            rusqlite::params![release_id,source], |row| row.get(0)).unwrap_or(false))
    }

    pub fn finished_step_bits(&self, release_id: i64, mask: super::step::StepMask) -> Result<super::step::StepMask, StepError> {
        self.legacy()?.query_row("select coalesce((select done|failed from pp_state where release_id=?1),0)&?2",rusqlite::params![release_id,mask], |row| row.get(0))
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn beside_par2_files(&self, release_id: i64) -> Result<std::collections::HashSet<Vec<u8>>, StepError> {
        self.legacy()?.prepare_cached("select par2_file from beside_files where release_id=?1")
            .and_then(|mut query| query.query_map([release_id], |row| row.get::<_,Option<Vec<u8>>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()).map(|rows| rows.into_iter().flatten().collect())
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn upload_members(&self, lead: i64) -> Result<Vec<(i64,Option<Vec<u8>>)>, StepError> {
        self.legacy()?.prepare_cached("select release_id,par2_file from upload_link where lead_id=?1")
            .and_then(|mut query| query.query_map([lead], |row| Ok((row.get(0)?,row.get(1)?)))?.collect())
            .map_err(|error| StepError::Retry(error.to_string()))
    }

    pub(crate) fn link_beside_candidates(&self, release_id: i64, name: &str, window: (&str,&str,&str)) -> Result<Vec<crate::archive::link::Candidate>, StepError> {
        crate::archive::link::LinkStep::beside_candidates(self.legacy()?, release_id, name, window)
    }

    pub(crate) fn link_candidates(&self, release: &super::step::ReleaseInfo, sizes: (u64,u64), set: Option<[u8;16]>) -> Result<Vec<crate::archive::link::Candidate>, StepError> {
        crate::archive::link::LinkStep::candidates_legacy(self.legacy()?, release, sizes, set)
    }

    pub fn beside_rows_with_files(&self, release_id: i64) -> Result<Vec<(i64,crate::store::FileInfo)>, StepError> {
        crate::archive::beside::own_beside_legacy(self.legacy()?, release_id)
    }

    pub fn find_beside_nfo(&self, release_id: i64, stem: &str, posted: Option<&str>) -> Result<Option<(i64,crate::store::FileInfo)>, StepError> {
        crate::archive::beside::nfo_beside(self.legacy()?, release_id, stem, posted)
    }

    pub fn find_nfo_owner(&self, release_id: i64, stem: &str, posted: Option<&str>) -> Result<Option<i64>, StepError> {
        crate::archive::beside::nfo_owner(self.legacy()?, release_id, stem, posted)
    }

    pub fn post_posters(&self, name: &str, stem: &str, own: Option<&str>, posted: &str) -> Result<(Vec<String>,Vec<i64>), StepError> {
        crate::archive::beside::post_posters(self.legacy()?, name, stem, own, posted)
    }

    pub fn counter_files(&self, release_id: i64) -> Result<Vec<crate::store::FileInfo>, StepError> {
        crate::store::release_files(self.legacy()?, release_id, 64).map_err(|error| StepError::Retry(error.to_string()))
    }

    pub fn find_thumbnail(&self, release_id: i64, total: i64, slots: &std::collections::HashSet<i64>, posters: &[String], posted: &str) -> Result<Option<i64>, StepError> {
        crate::archive::beside::thumbnail_beside(self.legacy()?, release_id, total, slots, posters, posted)
    }

    pub fn beside_owner_local(&self, row_id: i64) -> Result<Option<i64>, StepError> {
        self.legacy()?.query_row("select release_id from beside_files where row_id=?1", [row_id], |row| row.get(0))
            .optional().map_err(|error| StepError::Retry(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_reads_preserve_missing_negative_case_and_movie_semantics() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table release_probe(release_id integer primary key,moviehash text);
            create table release_opensubtitles(release_id integer primary key,moviehash text,imdb_id integer,kind text);
            insert into release_probe values(1,'AbCd'),(2,NULL);
            insert into release_opensubtitles values(1,'AbCd',101,'MoViE'),(2,'negative',NULL,NULL),(3,'episode',303,'episode');").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert_eq!(data.probe_moviehash(1).unwrap().as_deref(), Some("AbCd"));
        assert_eq!(data.probe_moviehash(2).unwrap(), None);
        assert_eq!(data.probe_moviehash(4).unwrap(), None);
        assert!(data.opensubtitles_asked(1, "AbCd").unwrap());
        assert!(!data.opensubtitles_asked(1, "abcd").unwrap());
        assert!(data.opensubtitles_asked(2, "negative").unwrap());
        assert_eq!(data.opensubtitles_movie_imdb(1).unwrap(), Some(101));
        assert_eq!(data.opensubtitles_movie_imdb(2).unwrap(), None);
        assert_eq!(data.opensubtitles_movie_imdb(3).unwrap(), None);
    }

    #[test]
    fn failed_reads_and_unavailable_native_capability_never_become_absence() {
        let conn = Connection::open_in_memory().unwrap();
        for data in [StepData::LegacySqlite(&conn), StepData::NativeUnavailable] {
            assert!(matches!(data.probe_moviehash(1), Err(StepError::Retry(_))));
            assert!(matches!(data.opensubtitles_asked(1, "hash"), Err(StepError::Retry(_))));
            assert!(matches!(data.opensubtitles_movie_imdb(1), Err(StepError::Retry(_))));
            assert!(matches!(data.category_exists(1), Err(StepError::Retry(_))));
            assert!(matches!(data.category_spot(1), Err(StepError::Retry(_))));
            assert!(matches!(data.probe_info(1), Err(StepError::Retry(_))));
        }
        assert!(matches!(StepData::NativeUnavailable.explicit_rerun(1, super::super::step::StepId(1)), Err(StepError::Retry(_))));
        assert!(matches!(StepData::NativeUnavailable.sidecar_text_bytes(), Err(StepError::Retry(_))));
        assert!(matches!(StepData::NativeUnavailable.legacy_connection(), Err(StepError::Retry(_))));
        assert!(StepData::NativeUnavailable.release_files(1).is_err());
        assert!(StepData::NativeUnavailable.file_segments(1,&crate::store::SegmentPick::First(1)).is_err());
    }

    #[test]
    fn checked_category_reads_preserve_spot_kind_subcats_and_missing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table release_category(release_id integer primary key,category integer);
            create table spots(release_id integer primary key,kind text,subcats text);
            insert into release_category values(1,2040);
            insert into spots values(1,'movie','a1|d11|'),(2,'series',NULL),(3,'book','a1|');").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(data.category_exists(1).unwrap());
        assert!(!data.category_exists(2).unwrap());
        let (kind, subcats) = data.category_spot(1).unwrap().unwrap();
        assert_eq!(kind, crate::spotnet::Kind::Movie);
        assert_eq!(subcats.d, [11]);
        assert_eq!(data.category_spot(2).unwrap().unwrap().0, crate::spotnet::Kind::Series);
        assert!(data.category_spot(3).unwrap().is_none());
        assert!(data.category_spot(4).unwrap().is_none());
    }

    #[test]
    fn checked_probe_reads_keep_json_and_legacy_rerun_marker_semantics() {
        let conn = Connection::open_in_memory().unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(!data.explicit_rerun(1, super::super::step::StepId(10)).unwrap());
        conn.execute_batch("create table release_probe(release_id integer primary key,info text);
            create table pp_state(release_id integer primary key,rerun_mask integer);
            insert into release_probe values(1,'{\"file\":\"example.mkv\"}');
            insert into pp_state values(1,1024);").unwrap();
        assert_eq!(data.probe_info(1).unwrap().as_deref(), Some("{\"file\":\"example.mkv\"}"));
        assert_eq!(data.probe_info(2).unwrap(), None);
        assert!(data.explicit_rerun(1, super::super::step::StepId(10)).unwrap());
        assert!(!data.explicit_rerun(1, super::super::step::StepId(5)).unwrap());
    }

    #[test]
    fn checked_sidecar_accounting_retains_legacy_deferred_eviction_policy() {
        let conn = Connection::open_in_memory().unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert_eq!(data.sidecar_text_bytes().unwrap(), 0);
        conn.execute_batch("create table meta(key text primary key,value integer);
            insert into meta values('sidecar_text_bytes',123)").unwrap();
        assert_eq!(data.sidecar_text_bytes().unwrap(), 123);
    }

    #[test]
    fn checked_naming_and_nfo_reads_do_not_admit_unavailable_native_data() {
        let data = StepData::NativeUnavailable;
        assert!(matches!(data.archive_inner_files(1), Err(StepError::Retry(_))));
        assert!(matches!(data.archive_password(1), Err(StepError::Retry(_))));
        assert!(matches!(data.rename_named("example.group"), Err(StepError::Retry(_))));
        assert!(matches!(data.release_content_id(1), Err(StepError::Retry(_))));
        assert!(matches!(data.nfo_password(1), Err(StepError::Retry(_))));
        assert!(matches!(data.local_password_check(1), Err(StepError::Retry(_))));
        assert!(matches!(data.stored_nfo(1), Err(StepError::Retry(_))));
        assert!(matches!(data.par2_names(1, &[]), Err(StepError::Retry(_))));
        assert!(matches!(data.nfo_password_rerun_flags(1), Err(StepError::Retry(_))));
        assert!(matches!(data.archive_requires_password_list(1), Err(StepError::Retry(_))));
        assert!(matches!(data.par2_file_names(1), Err(StepError::Retry(_))));
        assert!(matches!(data.same_mux_media(1,&crate::probe::ProbeInfo::default(),0), Err(StepError::Retry(_))));
        assert!(matches!(data.linked_image_files(1), Err(StepError::Retry(_))));
        assert!(matches!(data.requested_image_file(1,"example.jpg"), Err(StepError::Retry(_))));
        assert!(matches!(data.image_requests(1), Err(StepError::Retry(_))));
        assert!(matches!(data.kept_images(1), Err(StepError::Retry(_))));
        assert!(matches!(data.archive_source_name_bytes(1,1), Err(StepError::Retry(_))));
        assert!(matches!(data.archive_total_name_bytes(), Err(StepError::Retry(_))));
        assert!(matches!(data.legacy_step_key_salt(), Err(StepError::Retry(_))));
        assert!(matches!(data.upload_lead(1), Err(StepError::Retry(_))));
        assert!(matches!(data.upload_tried_count(1), Err(StepError::Retry(_))));
        assert!(matches!(data.release_size(1), Err(StepError::Retry(_))));
        assert!(matches!(data.upload_is_lead(1), Err(StepError::Retry(_))));
        assert!(matches!(data.release_name_marker(1), Err(StepError::Retry(_))));
        assert!(matches!(data.posted_age_days("2000-01-01"), Err(StepError::Retry(_))));
        assert!(matches!(data.file_clock(1), Err(StepError::Retry(_))));
        assert!(matches!(data.par2_inventory(1), Err(StepError::Retry(_))));
        assert!(matches!(data.archive_source_exists(1,1), Err(StepError::Retry(_))));
        assert!(matches!(data.finished_step_bits(1,1), Err(StepError::Retry(_))));
        assert!(matches!(data.beside_par2_files(1), Err(StepError::Retry(_))));
        assert!(matches!(data.upload_members(1), Err(StepError::Retry(_))));
        assert!(matches!(data.link_beside_candidates(1,"example",("from","to","posted")), Err(StepError::Retry(_))));
        assert!(matches!(data.link_candidates(&super::super::step::ReleaseInfo::default(),(1,2),None), Err(StepError::Retry(_))));
        assert!(matches!(data.beside_rows_with_files(1), Err(StepError::Retry(_))));
        assert!(matches!(data.find_beside_nfo(1,"example",None), Err(StepError::Retry(_))));
        assert!(matches!(data.find_nfo_owner(1,"example",None), Err(StepError::Retry(_))));
        assert!(matches!(data.post_posters("example","example",None,"2000-01-01"), Err(StepError::Retry(_))));
        assert!(matches!(data.counter_files(1), Err(StepError::Retry(_))));
        assert!(matches!(data.find_thumbnail(1,2,&std::collections::HashSet::new(),&[],"2000-01-01"), Err(StepError::Retry(_))));
        assert!(matches!(data.beside_owner_local(1), Err(StepError::Retry(_))));
        assert!(matches!(data.media_imdb_marker(1), Err(StepError::Retry(_))));
        assert!(matches!(data.media_exists(1), Err(StepError::Retry(_))));
        assert!(matches!(data.saved_probe_media(1), Err(StepError::Retry(_))));
        assert!(matches!(data.stale_media_ids(1), Err(StepError::Retry(_))));
        assert!(matches!(data.predb_catalog_path(), Err(StepError::Retry(_))));
        assert!(matches!(data.prior_predb_match(1), Err(StepError::Retry(_))));
        assert!(matches!(data.key_salt(Some(42)), Err(StepError::Retry(_))));
        assert!(matches!(data.release_keys(1,None,42), Err(StepError::Retry(_))));
        assert!(matches!(data.stored_release_keys(1), Err(StepError::Retry(_))));
        assert!(matches!(data.probe_info_best_effort(1), Err(StepError::Retry(_))));
        assert!(matches!(data.kept_stills(1), Err(StepError::Retry(_))));
        assert!(matches!(data.has_audio_print(1), Err(StepError::Retry(_))));
        assert!(matches!(data.legacy_main_path(), Err(StepError::Retry(_))));
        assert!(matches!(data.release_lookup(1,None), Err(StepError::Retry(_))));
        assert!(matches!(data.stills_lookup(1), Err(StepError::Retry(_))));
    }

    #[test]
    fn checked_naming_opt_in_and_media_imdb_keep_presence_semantics() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table meta(key text primary key,value integer);
            create table media_release(release_id integer primary key,imdb_id integer);
            insert into media_release values(1,NULL),(2,202)").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(!data.rename_named("example.group").unwrap());
        conn.execute("insert into meta values(?1,0)", [crate::postprocess::reprocess::rename_named_key("example.group")]).unwrap();
        assert!(data.rename_named("example.group").unwrap());
        assert!(!data.rename_named("other.group").unwrap());
        conn.execute("insert into meta values('naming_rename_named',0)", []).unwrap();
        assert!(data.rename_named("other.group").unwrap());
        assert_eq!(data.media_imdb_marker(1).unwrap(), Some(None));
        assert_eq!(data.media_imdb_marker(2).unwrap(), Some(Some(202)));
        assert_eq!(data.media_imdb_marker(3).unwrap(), None);
        assert!(data.media_exists(1).unwrap());
        assert!(!data.media_exists(3).unwrap());
        assert_eq!(data.nfo_password_rerun_flags(1).unwrap(), (false,false));
    }

    #[test]
    fn checked_predb_match_keeps_missing_distinct_from_reader_failure() {
        let conn = Connection::open_in_memory().unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(matches!(data.prior_predb_match(1), Err(StepError::Retry(_))));
        assert!(matches!(data.predb_catalog_path(), Err(StepError::Retry(_))));
        conn.execute_batch(crate::predb::step::MATCH.create).unwrap();
        assert_eq!(data.prior_predb_match(1).unwrap(), None);
        conn.execute("insert into predb_match(release_id,title_id,kind,matched_at) values(1,9,1,100)", []).unwrap();
        assert_eq!(data.prior_predb_match(1).unwrap(), Some(9));
    }

    #[test]
    fn checked_still_and_audio_reads_retain_legacy_error_and_presence_policies() {
        let conn = Connection::open_in_memory().unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(matches!(data.kept_stills(1), Err(StepError::Retry(_))));
        assert!(!data.has_audio_print(1).unwrap());
        assert_eq!(data.probe_info_best_effort(1).unwrap(),None);
        conn.execute_batch("create table release_stills(release_id integer,sha256 text,phash integer);
            create table release_audio_print(release_id integer);
            insert into release_stills values(1,'unhashed',NULL),(1,'hashed',-1);
            insert into release_audio_print values(1)").unwrap();
        assert_eq!(data.kept_stills(1).unwrap(),vec![("unhashed".into(),false),("hashed".into(),true)]);
        assert!(data.has_audio_print(1).unwrap());
        assert!(!data.has_audio_print(2).unwrap());
    }

    #[test]
    fn checked_password_list_hint_preserves_hidden_and_nested_nzb_source_rules() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table archive_release(release_id integer,names_hidden integer,password integer);
            create table archive_inner_files(release_id integer,source integer,name text);
            insert into archive_release values(1,1,0),(2,1,0),(3,0,1),(4,0,1);
            insert into archive_inner_files values(2,1,'example.mkv'),(3,1,'inside.nzb'),(4,1,'inside.nzb'),(4,5,'unpacked.mkv');").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert!(data.archive_requires_password_list(1).unwrap());
        assert!(!data.archive_requires_password_list(2).unwrap());
        assert!(data.archive_requires_password_list(3).unwrap());
        assert!(!data.archive_requires_password_list(4).unwrap());
    }

    #[test]
    fn checked_image_requests_keep_pending_and_shown_rows_only() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::images::step::PICTURE_REQUESTS.create).unwrap();
        conn.execute_batch("insert into picture_requests(release_id,file,requested_at,done_at,outcome) values
            (1,'pending.jpg',1,NULL,NULL),(1,'shown.jpg',1,2,'shown'),(1,'failed.jpg',1,2,'unreadable'),(2,'other.jpg',1,NULL,NULL)").unwrap();
        let mut asked = StepData::LegacySqlite(&conn).image_requests(1).unwrap();
        asked.sort();
        assert_eq!(asked,vec!["pending.jpg".to_string(),"shown.jpg".to_string()]);
    }

    #[test]
    fn checked_archive_name_accounting_keeps_byte_units_source_scope_and_missing_meta() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table archive_inner_files(release_id integer,name text,source integer);
            create table meta(key text primary key,value integer);
            insert into archive_inner_files values(1,'é',2),(1,'other',1)").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert_eq!(data.archive_source_name_bytes(1,2).unwrap(),34);
        assert_eq!(data.archive_source_name_bytes(2,2).unwrap(),0);
        assert_eq!(data.archive_total_name_bytes().unwrap(),0);
        conn.execute("insert into meta values('archive_name_bytes',34)", []).unwrap();
        assert_eq!(data.archive_total_name_bytes().unwrap(),34);
    }

    #[test]
    fn checked_upload_reads_keep_null_name_membership_and_finished_bits() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::archive::link::UPLOAD_LINK.create).unwrap();
        conn.execute_batch(crate::archive::link::UPLOAD_LINK_TRIED.create).unwrap();
        conn.execute_batch("create table releases(id integer primary key,size integer,display_name text);
            create table pp_state(release_id integer primary key,done integer,failed integer);
            create table file_timing(file_id integer,first_date integer,last_date integer,clock_first integer,clock_last integer);
            insert into releases values(1,NULL,NULL),(2,100,'named');
            insert into upload_link(release_id,lead_id,par2_file) values(1,1,NULL),(2,1,x'AB');
            insert into upload_link_tried values(1,3),(1,4);
            insert into pp_state values(1,2,4);
            insert into file_timing values(1,5,9,1000,2000)").unwrap();
        let data = StepData::LegacySqlite(&conn);
        assert_eq!(data.upload_lead(2).unwrap(),Some(1));
        assert_eq!(data.upload_lead(3).unwrap(),None);
        assert!(data.upload_is_lead(1).unwrap());
        assert!(!data.upload_is_lead(2).unwrap());
        assert_eq!(data.upload_tried_count(1).unwrap(),2);
        assert_eq!(data.release_size(1).unwrap(),Some(0));
        assert_eq!(data.release_size(3).unwrap(),None);
        assert_eq!(data.release_name_marker(1).unwrap(),Some(None));
        assert_eq!(data.release_name_marker(2).unwrap(),Some(Some("named".into())));
        assert_eq!(data.release_name_marker(3).unwrap(),None);
        let mut members = data.upload_members(1).unwrap(); members.sort_by_key(|row| row.0);
        assert_eq!(members,vec![(1,None),(2,Some(vec![0xAB]))]);
        assert_eq!(data.finished_step_bits(1,7).unwrap(),6);
        assert_eq!(data.file_clock(1).unwrap(),Some(crate::archive::link::Clock { start: 1.0,end: 2.0 }));
    }
}
