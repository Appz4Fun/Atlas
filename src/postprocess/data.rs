//! Checked reads from the current canonical native owner. Mutation authority
//! remains with the lease and Observed participant protocol, never this view.
use super::step::StepError;

#[derive(Debug)]
pub(crate) struct NativeReadUnavailable;
impl std::fmt::Display for NativeReadUnavailable {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Step read scope is unavailable")}
}
impl std::error::Error for NativeReadUnavailable {}

pub enum StepData<'a> {
    Native(&'a crate::pg::stage_reads::StepReads),
    /// An absent native capability is an error, never empty inventory or a miss.
    NativeUnavailable,
}
impl StepData<'_> {
    fn native(&self)->anyhow::Result<&crate::pg::stage_reads::StepReads> {
        match self {Self::Native(reads)=>Ok(reads),Self::NativeUnavailable=>Err(NativeReadUnavailable.into())}
    }
    pub fn release_files(&self, release_id: i64) -> anyhow::Result<Vec<crate::store::FileInfo>> {
        let native = self.native()?;
        native.files(release_id,5000)
    }

    pub fn file_segments(&self, file_id: i64, pick: &crate::store::SegmentPick) -> anyhow::Result<Vec<crate::store::SegmentRef>> {
        let native = self.native()?;
        native.segments(file_id,pick)
    }

    pub fn probe_moviehash(&self, release_id: i64) -> Result<Option<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.probe_moviehash(release_id).map_err(StepError::Native)
    }

    pub fn opensubtitles_asked(&self, release_id: i64, hash: &str) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.opensubtitles_asked(release_id,hash).map_err(StepError::Native)
    }

    pub fn opensubtitles_movie_imdb(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.opensubtitles_movie_imdb(release_id).map_err(StepError::Native)
    }

    pub fn category_exists(&self, release_id: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.category_exists(release_id).map_err(StepError::Native)
    }

    pub fn category_spot(&self, release_id: i64) -> Result<Option<(crate::spotnet::Kind, crate::spotnet::Subcats)>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.category_spot(release_id).map_err(StepError::Native)
    }

    pub fn probe_info(&self, release_id: i64) -> Result<Option<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.probe_info(release_id).map_err(StepError::Native)
    }

    pub fn explicit_rerun(&self, release_id: i64, step: super::step::StepId) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.explicit_rerun(release_id,step).map_err(StepError::Native)
    }

    pub fn sidecar_text_bytes(&self) -> Result<i64, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.sidecar_text_bytes().map_err(StepError::Native)
    }

    pub fn archive_inner_files(&self, release_id: i64) -> Result<Vec<crate::archive::store::InnerFileRow>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_inner_files(release_id).map_err(StepError::Native)
    }

    pub(crate) fn xref_tried(&self,id:i64,name:&str)->Result<Option<(i64,i64)>,StepError> {
        let native = self.native().map_err(StepError::Native)?;
        match native.xref_tried(id,name).map_err(StepError::Native)? {
            crate::pg::stage_reads::xref::WindowRead::Publishing=>Err(StepError::Transient("native archive metadata publication pending".into())),
            crate::pg::stage_reads::xref::WindowRead::Ready(probed,generation)=>Ok(Some((probed,generation))),
        }
    }

    pub(crate) fn xref_leads(&self,id:i64,name:&str,file:i64,generation:Option<i64>)->Result<Vec<i64>,StepError> {
        let native = self.native().map_err(StepError::Native)?;
        match native.xref_leads(id,name,file,generation.ok_or_else(||StepError::Native(NativeReadUnavailable.into()))?).map_err(StepError::Native)? {
            crate::pg::stage_reads::xref::LeadRead::Changed=>Err(StepError::Transient("native archive probe window changed".into())),
            crate::pg::stage_reads::xref::LeadRead::Found(parts)=>Ok(parts),
        }
    }

    pub fn archive_password(&self, release_id: i64) -> Result<Option<crate::archive::Password>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_password(release_id).map_err(StepError::Native)
    }

    pub fn rename_named(&self, group: &str) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.rename_named(group).map_err(StepError::Native)
    }

    pub fn release_content_id(&self, release_id: i64) -> Result<Option<Vec<u8>>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.content_id(release_id).map_err(StepError::Native)
    }

    pub fn nfo_password(&self, release_id: i64) -> Result<Option<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.nfo_password(release_id).map_err(StepError::Native)
    }

    pub fn local_password_check(&self, release_id: i64) -> Result<Option<crate::archive::PasswordTest>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.local_password_check(release_id).map_err(StepError::Native)
    }

    pub fn stored_nfo(&self, release_id: i64) -> Result<Option<crate::probe::nfo_store::StoredNfo>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.stored_nfo(release_id).map_err(StepError::Native)
    }

    pub fn par2_names(&self, release_id: i64, files: &[crate::store::FileInfo]) -> Result<std::collections::HashMap<String,String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.par2_names(release_id, files).map_err(StepError::Native)
    }

    pub fn archive_requires_password_list(&self, release_id: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_requires_password_list(release_id).map_err(StepError::Native)
    }

    pub fn nfo_password_rerun_flags(&self, release_id: i64) -> Result<(bool,bool), StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.nfo_password_rerun_flags(release_id).map_err(StepError::Native)
    }

    pub fn media_imdb_marker(&self, release_id: i64) -> Result<Option<Option<i64>>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.media_imdb_marker(release_id).map_err(StepError::Native)
    }

    pub fn media_exists(&self, release_id: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.media_exists(release_id).map_err(StepError::Native)
    }

    pub fn saved_probe_media(&self, release_id: i64) -> Result<Option<crate::probe::media::MediaInfo>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.saved_probe_media(release_id).map_err(StepError::Native)
    }

    pub fn stale_media_ids(&self, title_key: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.stale_media_ids(title_key).map_err(StepError::Native)
    }

    pub(crate) fn predb_find(&self,evidence:&crate::predb::matcher::Evidence<'_>)->Result<crate::pg::predb_cache::LookupStatus,StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.predb_find(evidence).map_err(StepError::Native)
    }

    pub fn prior_predb_match(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.prior_predb_match(release_id).map_err(StepError::Native)
    }

    pub fn key_salt(&self, cached: Option<i64>) -> Result<i64, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.key_salt(cached).map_err(StepError::Native)
    }

    pub fn release_keys(&self, release_id: i64, parts: Option<i64>, salt: i64) -> Result<crate::keys::Keys, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.release_keys(release_id,parts,salt).map_err(StepError::Native)
    }

    pub fn stored_release_keys(&self, release_id: i64) -> Result<Option<crate::keys::Keys>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.stored_release_keys(release_id).map_err(StepError::Native)
    }

    pub fn probe_info_best_effort(&self, release_id: i64) -> Result<Option<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.probe_info(release_id).map_err(StepError::Native)
    }

    pub fn kept_stills(&self, release_id: i64) -> Result<Vec<(String,bool)>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.kept_stills(release_id).map_err(StepError::Native)
    }

    pub fn has_audio_print(&self, release_id: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.has_audio_print(release_id).map_err(StepError::Native)
    }

    pub fn audio_linked_by(&self,release_id:i64,lookup:&crate::keys::Lookup)->Result<Option<crate::keys::Kind>,StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.audio_linked_by(release_id,lookup).map_err(StepError::Native)
    }

    pub fn same_file_stills(&self,release_id:i64,lookup:&crate::keys::Lookup)->Result<Option<crate::keys::Kind>,StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.same_file_stills(release_id,lookup).map_err(StepError::Native)
    }

    pub fn release_lookup(&self, release_id: i64, parts: Option<i64>) -> Result<crate::keys::Lookup, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.release_lookup(release_id,parts).map_err(StepError::Native)
    }

    pub fn stills_lookup(&self, release_id: i64) -> Result<crate::keys::Lookup, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.stills_lookup(release_id).map_err(StepError::Native)
    }

    pub fn par2_file_names(&self, release_id: i64) -> Result<Vec<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.par2_file_names(release_id).map_err(StepError::Native)
    }

    pub(crate) fn same_mux_media(&self, release_id: i64, info: &crate::probe::ProbeInfo, stamp: u64) -> Result<Option<crate::probe::step::SameMux>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.same_mux_media(release_id, info, stamp).map_err(StepError::Native)
    }

    pub fn linked_image_files(&self, release_id: i64) -> Result<Vec<crate::store::FileInfo>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.linked_image_files(release_id).map_err(StepError::Native)
    }

    pub fn requested_image_file(&self, release_id: i64, name: &str) -> Result<Option<crate::store::FileInfo>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.requested_image_file(release_id, name).map_err(StepError::Native)
    }

    pub fn image_requests(&self, release_id: i64) -> Result<Vec<String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.image_requests(release_id).map_err(StepError::Native)
    }

    pub fn kept_images(&self, release_id: i64) -> Result<std::collections::HashMap<String,String>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.kept_images(release_id).map_err(StepError::Native)
    }

    pub fn archive_source_name_bytes(&self, release_id: i64, source: i64) -> Result<i64, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_source_name_bytes(release_id,source).map_err(StepError::Native)
    }

    pub fn archive_total_name_bytes(&self) -> Result<i64, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_total_name_bytes().map_err(StepError::Native)
    }

    pub fn legacy_step_key_salt(&self) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.legacy_step_key_salt().map_err(StepError::Native)
    }

    pub fn upload_lead(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.upload_lead(release_id).map_err(StepError::Native)
    }

    pub fn upload_tried_count(&self, release_id: i64) -> Result<i64, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.upload_tried_count(release_id).map_err(StepError::Native)
    }

    pub fn release_size(&self, release_id: i64) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.release_size(release_id).map_err(StepError::Native)
    }

    pub fn upload_is_lead(&self, release_id: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.upload_is_lead(release_id).map_err(StepError::Native)
    }

    pub fn release_name_marker(&self, release_id: i64) -> Result<Option<Option<String>>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.release_name_marker(release_id).map_err(StepError::Native)
    }

    pub fn posted_age_days(&self, posted: &str) -> Result<Option<f64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.posted_age_days(posted).map_err(StepError::Native)
    }

    pub fn file_clock(&self, file_id: i64) -> Result<Option<crate::archive::link::Clock>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.file_clock(file_id).map_err(StepError::Native)
    }

    pub fn par2_inventory(&self, release_id: i64) -> Result<Option<crate::par2::Inventory>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.par2_inventory(release_id).map_err(StepError::Native)
    }

    pub fn archive_source_exists(&self, release_id: i64, source: i64) -> Result<bool, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.archive_source_exists(release_id,source).map_err(StepError::Native)
    }

    pub fn finished_step_bits(&self, release_id: i64, mask: super::step::StepMask) -> Result<super::step::StepMask, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.finished_step_bits(release_id,mask).map_err(StepError::Native)
    }

    pub fn beside_par2_files(&self, release_id: i64) -> Result<std::collections::HashSet<Vec<u8>>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.beside_par2_files(release_id).map_err(StepError::Native)
    }

    pub fn upload_members(&self, lead: i64) -> Result<Vec<(i64,Option<Vec<u8>>)>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.upload_members(lead).map_err(StepError::Native)
    }

    pub(crate) fn link_beside_candidates(&self, release_id: i64, name: &str, window: (&str,&str,&str)) -> Result<Vec<crate::archive::link::Candidate>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.link_beside_candidates(release_id, name, window).map_err(StepError::Native)
    }

    pub(crate) fn link_candidates(&self, release: &super::step::ReleaseInfo, sizes: (u64,u64), set: Option<[u8;16]>) -> Result<Vec<crate::archive::link::Candidate>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.link_candidates(release, sizes, set).map_err(StepError::Native)
    }

    pub fn beside_rows_with_files(&self, release_id: i64) -> Result<Vec<(i64,crate::store::FileInfo)>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.beside_rows_with_files(release_id).map_err(StepError::Native)
    }

    pub fn find_beside_nfo(&self, release_id: i64, stem: &str, posted: Option<&str>) -> Result<Option<(i64,crate::store::FileInfo)>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.find_beside_nfo(release_id, stem, posted).map_err(StepError::Native)
    }

    pub fn find_nfo_owner(&self, release_id: i64, stem: &str, posted: Option<&str>) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.find_nfo_owner(release_id, stem, posted).map_err(StepError::Native)
    }

    pub fn post_posters(&self, name: &str, stem: &str, own: Option<&str>, posted: &str) -> Result<(Vec<String>,Vec<i64>), StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.post_posters(name, stem, own, posted).map_err(StepError::Native)
    }

    pub fn counter_files(&self, release_id: i64) -> Result<Vec<crate::store::FileInfo>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.files(release_id,64).map_err(StepError::Native)
    }

    pub fn find_thumbnail(&self, release_id: i64, total: i64, slots: &std::collections::HashSet<i64>, posters: &[String], posted: &str) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.find_thumbnail(release_id, total, slots, posters, posted).map_err(StepError::Native)
    }

    pub fn beside_owner_local(&self, row_id: i64) -> Result<Option<i64>, StepError> {
        let native = self.native().map_err(StepError::Native)?;
        native.beside_owner_local(row_id).map_err(StepError::Native)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_errors_keep_original_driver_and_local_io_provenance() {
        let cases=[
            (anyhow::Error::new(crate::pg::outage::SessionUnavailable).context("native inventory"),crate::pg::outage::Failure::Unavailable),
            (anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset)).context("local inventory asset"),crate::pg::outage::Failure::Fatal),
        ];
        for (original,expected) in cases {
            let StepError::Native(error)=StepError::from_inventory(original) else {panic!("native inventory error became a release retry")};
            assert_eq!(crate::pg::outage::classify(&error),expected);
        }
    }
    #[test]
    fn unavailable_inventory_is_typed_error_without_database_compatibility() {
        let data=StepData::NativeUnavailable;
        let files:anyhow::Result<Vec<crate::store::FileInfo>>=data.release_files(7);
        let segments:anyhow::Result<Vec<crate::store::SegmentRef>>=data.file_segments(8,&crate::store::SegmentPick::First(1));
        assert!(files.unwrap_err().is::<NativeReadUnavailable>());
        assert!(segments.unwrap_err().is::<NativeReadUnavailable>());
        for error in [data.category_exists(7).map(|_|()).unwrap_err(),data.probe_info(7).map(|_|()).unwrap_err()] {
            let StepError::Native(error)=error else {panic!("missing owner must not become a cache miss/retry")};
            assert!(error.is::<NativeReadUnavailable>());
        }
    }
}
