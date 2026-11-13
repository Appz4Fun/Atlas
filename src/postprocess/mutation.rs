//! Closed release-local result data. A mutation carries no release reference or write authority.

use super::result_value::Value;

use crate::probe::opensubtitles::Feature;

pub struct SidecarRecord {
    pub name: String,
    pub group: String,
    pub file: Option<String>,
    pub kind: String,
    pub release_name: Option<String>,
    pub segment_uid: Option<String>,
    pub text: Option<String>,
    pub at: i64,
}

pub struct ImageRecord {
    pub file: String,
    pub sha256: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub bytes: i64,
    pub meta: Option<String>,
    pub exif_id: Option<String>,
    pub document_id: Option<String>,
}

impl ImageRecord {
    pub(crate) fn v1_values(&self) -> Vec<Value> {
        vec![self.file.clone().into(),self.sha256.clone().into(),self.format.clone().into(),
            i64::from(self.width).into(),i64::from(self.height).into(),self.bytes.into(),
            self.meta.clone().into(),self.exif_id.clone().into(),self.document_id.clone().into()]
    }
}

pub enum StepMutation {
    OpenSubtitlesMatch { moviehash: String, feature: Option<Feature>, checked_at: i64 },
    /// A hash match pins a film's ID only while no earlier source has supplied one.
    PinOpenSubtitlesMovieImdb { imdb_id: i64 },
    SetCategory { category: i64 },
    ClearCategory,
    SetProbeInfo { json: String },
    KeepSidecar(SidecarRecord),
    EvictSidecarTexts { max_bytes: i64 },
    KeepNfo { file: String, text: String },
    SetNfoPassword { password: String },
    PinNfoMovieImdb { imdb_id: i64 },
    ClearMedia,
    FoundMediaIds { title_key: i64, ids: crate::media::resolve::Ids },
    /// Native cache IDs are checked on the original writer transaction, after
    /// preceding fanout; no legacy Media object or late callback is captured.
    ParseMedia { kind:crate::media::title::Kind,title_key:i64,parsed:crate::media::parse::Parsed },
    ReindexWords,
    RecordSegmentCrc { message_id: String, decoded: i64, crc32: u32 },
    ClearPredbInnerKeys,
    AddPredbInnerKey { key: i64 },
    PredbRead { revision:i64 },
    XrefRead { generation:i64 },
    XrefTried { generation:i64,count:i64 },
    KeepPredbMatch { title_id: i64, kind: crate::predb::matcher::Kind, category: Option<String>, pre_at: Option<i64>, nuked: i64, matched_at: i64 },
    KeepReleaseKeys { keys: crate::keys::Keys },
    ClearReleaseKeys,
    ClearStills,
    KeepStill(crate::raw_store::SavedStill),
    ClearAudioPrints,
    ClearAudioAnchors,
    KeepAudioPrint { position: u8, at_ms: i64, items: Vec<u32> },
    KeepAudioAnchor { anchor: i64 },
    KeepProbe { crc32: Option<i64>, agent: Option<String>, json: String, moviehash: Option<String> },
    KeepCover { mime: String, sha256: String },
    ClearCover,
    DropNfoCopy { file: String },
    PinTagImdb { imdb_id: i64 },
    PinTagTmdb { tmdb_id: i64, kind: crate::media::title::Kind },
    PinTagTvdb { tvdb_id: i64 },
    ClearImages,
    KeepImage { position: i64, image: ImageRecord },
    AnswerImageRequest { file: String, done_at: i64, answer: crate::images::step::Answer },
    ClearArchiveSource { source: i64 },
    KeepArchiveFile { sequence: i64, source: i64, file: crate::archive::InnerFile },
    AdjustArchiveNameBytes { delta: i64 },
    ArchiveNamesOverQuota,
    SetArchivePassword { password: crate::archive::Password },
    SetArchiveEncryption { encryption: Option<crate::archive::Encryption>, names_hidden: bool, check: Option<crate::archive::PasswordTest> },
    SetArchiveKeys { keys: crate::archive::store::ArchiveKeys, facts: i64 },
    ClearRevVolumes,
    KeepRevVolume(crate::archive::rev::Volume),
    /// Legacy numeric participants are result data, never native release capabilities.
    RememberUploadCandidate { candidate_id: i64 },
    PlaceBesideFile { row_id: i64, owner_id: i64, par2_file: Option<Vec<u8>> },
    NameUploadLead { lead_id: i64, name: String, source: u8 },
    RerunUploadLead { lead_id: i64, clear: super::step::StepMask, net: super::step::StepMask },
    QueueUploadLead { lead_id: i64, due_at: i64 },
    MoveUpload { from_id: i64, to_id: i64 },
    LinkUploadData { member_id: i64, lead_id: i64, par2_file: [u8;16] },
    LinkUploadRecovery { lead_id: i64, wanted: i64, set_id: Option<[u8;16]> },
    ClearContentFingerprint,
    KeepNfoForLegacyTarget { target_id: i64, file: String, text: String },
    MarkPointed { name: String },
    RankPointed { name: String },
    RerunPointedList { name: String },
    RerunPointedProbe { name: String },
    SetPointedPassword { name: String, password: String },
    SetContentFingerprint { content: Vec<u8> },
    SetPostedFingerprint { posted: Vec<u8> },
    ClearPostedFingerprint,
    SetPar2Sizes { block: i64, blocks: i64 },
    ClearPar2Files,
    KeepPar2File { file: crate::par2::Par2File, sealed: bool, complete: bool },
    SetPar2Creator { set_id: [u8;16], creator: String },
    VouchComplete,
    DropPcCategory,
}

impl StepMutation {
    /// Cross-release targets still require the future provenance/epoch resolver. Numbers are not authority.
    pub(crate) fn requires_target_resolution(&self) -> bool {
        matches!(self, Self::RememberUploadCandidate { .. } | Self::PlaceBesideFile { .. }
            | Self::NameUploadLead { .. } | Self::RerunUploadLead { .. } | Self::QueueUploadLead { .. }
            | Self::MoveUpload { .. } | Self::LinkUploadData { .. } | Self::LinkUploadRecovery { .. }
            | Self::KeepNfoForLegacyTarget { .. } | Self::MarkPointed { .. } | Self::RankPointed { .. }
            | Self::RerunPointedList { .. } | Self::RerunPointedProbe { .. } | Self::SetPointedPassword { .. })
    }

    /// Frozen v1 tag and neutral ordered fields for the retained receipt wire format.
    /// The tag spelling is receipt identity, not native SQL execution authority.
    pub(crate) fn v1_fields(&self, release_id: i64) -> (&'static str, Vec<Value>) {
        match self {
            Self::OpenSubtitlesMatch { moviehash, feature, checked_at } => {
                let f = feature.clone().unwrap_or_default();
                (
                    "insert or replace into release_opensubtitles
                     (release_id,moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name,checked_at)
                     values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    vec![release_id.into(),moviehash.clone().into(),f.imdb_id.into(),f.tmdb_id.into(),f.kind.into(),f.title.into(),
                        f.year.into(),f.season.into(),f.episode.into(),f.release_name.into(),(*checked_at).into()],
                )
            }
            Self::PinOpenSubtitlesMovieImdb { imdb_id } | Self::PinNfoMovieImdb { imdb_id } => (
                "update media_release set imdb_id=?2,imdb_from_nfo=1 where release_id=?1 and imdb_id is null",
                vec![release_id.into(), (*imdb_id).into()],
            ),
            Self::SetCategory { category } => (
                "insert or replace into release_category(release_id,category) values(?1,?2)",
                vec![release_id.into(), (*category).into()],
            ),
            Self::ClearCategory => ("delete from release_category where release_id=?1", vec![release_id.into()]),
            Self::SetProbeInfo { json } => (
                "update release_probe set info=?2 where release_id=?1", vec![release_id.into(), json.clone().into()],
            ),
            Self::KeepSidecar(record) => (super::result_v1_tags::SIDECAR_SET, vec![
                release_id.into(),record.name.clone().into(),record.group.clone().into(),record.file.clone().into(),
                record.kind.clone().into(),record.release_name.clone().into(),record.segment_uid.clone().into(),
                record.text.clone().into(),record.at.into(),
            ]),
            Self::EvictSidecarTexts { max_bytes } => (super::result_v1_tags::SIDECAR_EVICT, vec![release_id.into(), (*max_bytes).into()]),
            Self::KeepNfo { file, text } => (
                "insert or ignore into release_nfo(release_id,file,text) values(?1,?2,?3)",
                vec![release_id.into(),file.clone().into(),text.chars().take(super::result_v1_tags::PROBE_NFO_STORE_MAX_CHARS).collect::<String>().into()],
            ),
            Self::SetNfoPassword { password } => (super::result_v1_tags::ARCHIVE_STORE_SET_NFO_PASSWORD, vec![release_id.into(),password.clone().into()]),
            Self::ClearMedia => ("delete from media_release where release_id=?1", vec![release_id.into()]),
            Self::ParseMedia {kind,title_key,parsed} => {
                let number=|n:Option<i32>|n.map(i64::from).into();
                (super::result_v1_tags::MEDIA_STEPS_UPSERT,vec![release_id.into(),(*kind as i64).into(),(*title_key).into(),number(parsed.season),number(parsed.episode),number(parsed.episode_last),number(parsed.air_date),number(parsed.year),i64::from(parsed.quality).into(),Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null])
            },
            Self::FoundMediaIds { title_key, ids } => (super::result_v1_tags::MEDIA_STEPS_FOUND, vec![
                release_id.into(),(*title_key).into(),ids.tvdb.into(),ids.tvmaze.into(),ids.trakt.into(),ids.tmdb.into(),ids.imdb.into(),ids.tvrage.into(),
            ]),
            Self::ReindexWords => (
                "insert into word_queue(release_id) values(?1) on conflict(release_id) do update set n=n+1", vec![release_id.into()],
            ),
            Self::RecordSegmentCrc { message_id, decoded, crc32 } => (
                super::result_v1_tags::ARCHIVE_STORE_SET_SEGMENT_CRC,
                vec![release_id.into(),message_id.clone().into(),(*decoded).into(),i64::from(*crc32).into()],
            ),
            Self::ClearPredbInnerKeys => ("delete from predb_inner_key where release_id=?1", vec![release_id.into()]),
            Self::AddPredbInnerKey { key } => ("insert or ignore into predb_inner_key(key,release_id) values(?1,?2)", vec![(*key).into(),release_id.into()]),
            Self::PredbRead {revision} => (
                "select revision from ext_predb_key_meta where singleton=1 and revision=?1",vec![(*revision).into()],
            ),
            Self::XrefRead {generation} => (
                "select generation from linker_uploads where release_name=(select name from releases where id=?1) and generation=?2",vec![release_id.into(),(*generation).into()],
            ),
            Self::XrefTried {generation,count} => (
                "update linker_uploads set probed=max(probed,?3) where release_name=(select name from releases where id=?1) and generation=?2",vec![release_id.into(),(*generation).into(),(*count).into()],
            ),
            Self::KeepPredbMatch { title_id, kind, category, pre_at, nuked, matched_at } => (
                "insert or replace into predb_match(release_id,title_id,kind,category,pre_at,nuked,matched_at) values(?1,?2,?3,?4,?5,?6,?7)",
                vec![release_id.into(),(*title_id).into(),(*kind as i64).into(),category.clone().into(),(*pre_at).into(),(*nuked).into(),(*matched_at).into()],
            ),
            Self::KeepReleaseKeys { keys } => (super::result_v1_tags::KEYS_UPSERT, vec![
                release_id.into(),keys.account.into(),keys.host.into(),keys.layout.into(),keys.lines.into(),keys.file_crc.into(),
                keys.body_crc.into(),keys.video.into(),keys.audio.into(),keys.mux.into(),keys.encode.into(),keys.thread.into(),
                keys.subs.into(),keys.fonts.into(),keys.chapters.into(),keys.volumes.into(),
            ]),
            Self::ClearReleaseKeys => ("delete from release_keys where release_id=?1", vec![release_id.into()]),
            Self::ClearStills => ("delete from release_stills where release_id=?1", vec![release_id.into()]),
            Self::KeepStill(still) => (
                "insert or replace into release_stills(release_id,position,at_ms,sha256,width,height,phash) values(?1,?2,?3,?4,?5,?6,?7)",
                vec![release_id.into(),i64::from(still.position).into(),still.at_ms.into(),still.sha256.clone().into(),
                    i64::from(still.width).into(),i64::from(still.height).into(),still.phash.into()],
            ),
            Self::ClearAudioPrints => ("delete from release_audio_print where release_id=?1", vec![release_id.into()]),
            Self::ClearAudioAnchors => ("delete from release_audio_anchor where release_id=?1", vec![release_id.into()]),
            Self::KeepAudioPrint { position, at_ms, items } => (
                "insert or replace into release_audio_print(release_id,position,at_ms,items) values(?1,?2,?3,?4)",
                vec![release_id.into(),i64::from(*position).into(),(*at_ms).into(),crate::audio::to_blob(items).into()],
            ),
            Self::KeepAudioAnchor { anchor } => (
                "insert or ignore into release_audio_anchor(release_id,anchor) values(?1,?2)",vec![release_id.into(),(*anchor).into()],
            ),
            Self::KeepProbe { crc32, agent, json, moviehash } => (super::result_v1_tags::PROBE_SET_PROBE,
                vec![release_id.into(),(*crc32).into(),agent.clone().into(),json.clone().into(),moviehash.clone().into()]),
            Self::KeepCover { mime, sha256 } => (super::result_v1_tags::PROBE_SET_COVER,vec![release_id.into(),mime.clone().into(),sha256.clone().into()]),
            Self::ClearCover => (super::result_v1_tags::PROBE_DROP_COVER,vec![release_id.into()]),
            Self::DropNfoCopy { file } => (super::result_v1_tags::PROBE_NFO_STORE_DROP,vec![release_id.into(),file.clone().into()]),
            Self::PinTagImdb { imdb_id } => (
                "update media_release set imdb_id=?2,imdb_from_nfo=1 where release_id=?1 and kind=2",vec![release_id.into(),(*imdb_id).into()],
            ),
            Self::PinTagTmdb { tmdb_id, kind } => (
                "update media_release set tmdb_id=?2,tmdb_from_tag=1 where release_id=?1 and kind=?3",vec![release_id.into(),(*tmdb_id).into(),(*kind as i64).into()],
            ),
            Self::PinTagTvdb { tvdb_id } => (
                "update media_release set tvdb_id=?2,tvdb_from_tag=1 where release_id=?1 and kind=1",vec![release_id.into(),(*tvdb_id).into()],
            ),
            Self::ClearImages => ("delete from release_images where release_id=?1",vec![release_id.into()]),
            Self::KeepImage { position, image } => {
                let mut params = vec![release_id.into(),(*position).into()];
                params.extend(image.v1_values());
                ("insert or replace into release_images(release_id,position,file,sha256,format,width,height,bytes,meta,exif_id,document_id)
                  values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params)
            }
            Self::AnswerImageRequest { file, done_at, answer } => (
                "update picture_requests set done_at=?3,outcome=?4 where release_id=?1 and file=?2",
                vec![release_id.into(),file.clone().into(),(*done_at).into(),answer.word().to_string().into()],
            ),
            Self::ClearArchiveSource { source } => (super::result_v1_tags::ARCHIVE_STORE_DELETE_SOURCE,vec![release_id.into(),(*source).into()]),
            Self::KeepArchiveFile { sequence, source, file } => (super::result_v1_tags::ARCHIVE_STORE_INSERT_FILE_FACTS,
                vec![release_id.into(),(*sequence).into(),file.name.clone().into(),file.size.and_then(|size| i64::try_from(size).ok()).into(),
                    (*source).into(),file.crc32.map(i64::from).into(),file.mtime.into(),file.host_os.map(i64::from).into()]),
            Self::AdjustArchiveNameBytes { delta } => (super::result_v1_tags::ARCHIVE_STORE_ADD_NAME_BYTES,vec![(*delta).into()]),
            Self::ArchiveNamesOverQuota => (super::result_v1_tags::ARCHIVE_STORE_COUNT_OVER_QUOTA,vec![]),
            Self::SetArchivePassword { password } => (super::result_v1_tags::ARCHIVE_STORE_UPSERT_RELEASE,vec![release_id.into(),password.code().into()]),
            Self::SetArchiveEncryption { encryption, names_hidden, check } => (super::result_v1_tags::ARCHIVE_STORE_SET_ENCRYPTION,
                vec![release_id.into(),encryption.map(crate::archive::Encryption::code).into(),i64::from(*names_hidden).into(),
                    check.map(|check| check.to_bytes()).into()]),
            Self::SetArchiveKeys { keys, facts } => (super::result_v1_tags::ARCHIVE_STORE_SET_KEYS,
                vec![release_id.into(),keys.packer.clone().into(),keys.twin_key.into(),keys.owner_key.into(),keys.zone_key.into(),(*facts).into()]),
            Self::ClearRevVolumes => (super::result_v1_tags::ARCHIVE_REV_DELETE_REV_VOLUMES,vec![release_id.into()]),
            Self::KeepRevVolume(volume) => (super::result_v1_tags::ARCHIVE_REV_INSERT_REV_VOLUME,
                vec![release_id.into(),i64::from(volume.index).into(),i64::try_from(volume.size).unwrap_or(i64::MAX).into(),i64::from(volume.crc32).into()]),
            Self::RememberUploadCandidate { candidate_id } => (super::result_v1_tags::ARCHIVE_LINK_TRIED,vec![release_id.into(),(*candidate_id).into()]),
            Self::PlaceBesideFile { row_id, owner_id, par2_file } => (super::result_v1_tags::ARCHIVE_LINK_SET_BESIDE,
                vec![(*row_id).into(),(*owner_id).into(),par2_file.clone().into()]),
            Self::NameUploadLead { lead_id, name, source } => (super::result_v1_tags::ARCHIVE_LINK_NAME_LEAD,
                vec![(*lead_id).into(),name.clone().into(),i64::from(*source).into()]),
            Self::RerunUploadLead { lead_id, clear, net } => (super::result_v1_tags::ARCHIVE_LINK_RERUN_LEAD,
                vec![(*lead_id).into(),(*clear).into(),(*net).into()]),
            Self::QueueUploadLead { lead_id, due_at } => (super::result_v1_tags::ARCHIVE_LINK_QUEUE_LEAD,vec![(*lead_id).into(),(*due_at).into()]),
            Self::MoveUpload { from_id, to_id } => (super::result_v1_tags::ARCHIVE_LINK_MOVE_UPLOAD,vec![(*from_id).into(),(*to_id).into()]),
            Self::LinkUploadData { member_id, lead_id, par2_file } => (super::result_v1_tags::ARCHIVE_LINK_LINK_DATA,
                vec![(*member_id).into(),(*lead_id).into(),par2_file.to_vec().into()]),
            Self::LinkUploadRecovery { lead_id, wanted, set_id } => (super::result_v1_tags::ARCHIVE_LINK_LINK_RECOVERY,
                vec![release_id.into(),(*lead_id).into(),(*wanted).into(),set_id.map(|id| id.to_vec()).into()]),
            Self::ClearContentFingerprint => (super::result_v1_tags::DUPLICATES_CLEAR_CONTENT,vec![release_id.into()]),
            Self::KeepNfoForLegacyTarget { target_id, file, text } => (
                "insert or ignore into release_nfo(release_id,file,text) values(?1,?2,?3)",
                vec![(*target_id).into(),file.clone().into(),text.chars().take(super::result_v1_tags::PROBE_NFO_STORE_MAX_CHARS).collect::<String>().into()],
            ),
            Self::MarkPointed { name } => (super::result_v1_tags::ARCHIVE_STORE_MARK_POINTED,vec![name.clone().into(),release_id.into()]),
            Self::RankPointed { name } => (super::result_v1_tags::ARCHIVE_STORE_RANK_POINTED.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::RerunPointedList { name } => (super::result_v1_tags::ARCHIVE_STORE_RERUN_POINTED_LIST.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::RerunPointedProbe { name } => (super::result_v1_tags::ARCHIVE_STORE_RERUN_POINTED_PROBE.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::SetPointedPassword { name, password } => (super::result_v1_tags::ARCHIVE_STORE_MARK_POINTED_PASSWORD,vec![name.clone().into(),release_id.into(),password.clone().into()]),
            Self::SetContentFingerprint { content } => (super::result_v1_tags::DUPLICATES_SET_CONTENT,vec![release_id.into(),content.clone().into()]),
            Self::SetPostedFingerprint { posted } => (super::result_v1_tags::DUPLICATES_SET_POSTED,vec![release_id.into(),posted.clone().into()]),
            Self::ClearPostedFingerprint => (super::result_v1_tags::DUPLICATES_CLEAR_POSTED,vec![release_id.into()]),
            Self::SetPar2Sizes { block, blocks } => (super::result_v1_tags::DUPLICATES_SET_PAR2_SIZES,vec![release_id.into(),(*block).into(),(*blocks).into()]),
            Self::ClearPar2Files => (super::result_v1_tags::ARCHIVE_STORE_DELETE_PAR2_FILES,vec![release_id.into()]),
            Self::KeepPar2File { file, sealed, complete } => (super::result_v1_tags::ARCHIVE_STORE_INSERT_PAR2_FILE,
                vec![release_id.into(),file.file_id.to_vec().into(),file.set_id.to_vec().into(),file.md5.to_vec().into(),file.md5_16k.to_vec().into(),
                    i64::try_from(file.size).unwrap_or(i64::MAX).into(),file.name.clone().into(),file.crc32.map(i64::from).into(),
                    i64::from(file.bound).into(),i64::from(*sealed).into(),i64::from(*complete).into()]),
            Self::SetPar2Creator { set_id, creator } => (super::result_v1_tags::ARCHIVE_STORE_SET_PAR2_CREATOR,vec![release_id.into(),set_id.to_vec().into(),creator.clone().into()]),
            Self::VouchComplete => (super::result_v1_tags::STORE_SET_VOUCHED_COMPLETE,vec![release_id.into()]),
            Self::DropPcCategory => (super::result_v1_tags::JUNK_DROP_CATEGORY,vec![release_id.into()]),
        }
    }

}

#[cfg(test)]
#[path="mutation_protocol_tests.rs"]
mod tests;
#[cfg(test)]
#[path="mutation_native_tests.rs"]
mod native_tests;
