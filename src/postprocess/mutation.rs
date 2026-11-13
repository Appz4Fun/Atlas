//! Closed release-local result data. A mutation carries no release reference or write authority.

use rusqlite::types::Value;

use crate::probe::opensubtitles::Feature;

/// Deferred legacy cache resolution is explicit and cannot be submitted as a native mutation.
pub enum DeferredLegacyStepMutation {
    MediaParse { media: std::sync::Arc<crate::media::Media>, kind: crate::media::title::Kind,
        title_key: i64, parsed: crate::media::parse::Parsed },
}

impl DeferredLegacyStepMutation {
    pub(crate) fn sqlite_sql(&self) -> &'static str {
        match self { Self::MediaParse { .. } => crate::media::steps::UPSERT }
    }

    pub(crate) fn sqlite_parameters(self, release_id: i64) -> Vec<Value> {
        match self {
            Self::MediaParse { media, kind, title_key, parsed } => {
                // Evaluated on the legacy writer, after any preceding fan-out work, as before.
                let ids = media.cached_ids(title_key).unwrap_or_default();
                let number = |value: Option<i32>| value.map_or(Value::Null, |value| Value::Integer(i64::from(value)));
                vec![release_id.into(),(kind as i64).into(),title_key.into(),number(parsed.season),number(parsed.episode),
                    number(parsed.episode_last),number(parsed.air_date),number(parsed.year),i64::from(parsed.quality).into(),
                    ids.tvdb.into(),ids.tvmaze.into(),ids.trakt.into(),ids.tmdb.into(),ids.imdb.into(),ids.tvrage.into()]
            }
        }
    }
}

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
    pub(crate) fn sqlite_values(&self) -> Vec<Value> {
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

    /// Preserve the ordered legacy writer representation. This never executes native SQL.
    pub(crate) fn sqlite_statement(&self, release_id: i64) -> (&'static str, Vec<Value>) {
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
            Self::KeepSidecar(record) => (crate::sidecar::SET, vec![
                release_id.into(),record.name.clone().into(),record.group.clone().into(),record.file.clone().into(),
                record.kind.clone().into(),record.release_name.clone().into(),record.segment_uid.clone().into(),
                record.text.clone().into(),record.at.into(),
            ]),
            Self::EvictSidecarTexts { max_bytes } => (crate::sidecar::EVICT, vec![release_id.into(), (*max_bytes).into()]),
            Self::KeepNfo { file, text } => (
                "insert or ignore into release_nfo(release_id,file,text) values(?1,?2,?3)",
                vec![release_id.into(),file.clone().into(),text.chars().take(crate::probe::nfo_store::MAX_CHARS).collect::<String>().into()],
            ),
            Self::SetNfoPassword { password } => (crate::archive::store::SET_NFO_PASSWORD, vec![release_id.into(),password.clone().into()]),
            Self::ClearMedia => ("delete from media_release where release_id=?1", vec![release_id.into()]),
            Self::ParseMedia {kind,title_key,parsed} => {
                let number=|n:Option<i32>|n.map(i64::from).into();
                (crate::media::steps::UPSERT,vec![release_id.into(),(*kind as i64).into(),(*title_key).into(),number(parsed.season),number(parsed.episode),number(parsed.episode_last),number(parsed.air_date),number(parsed.year),i64::from(parsed.quality).into(),Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null])
            },
            Self::FoundMediaIds { title_key, ids } => (crate::media::steps::FOUND, vec![
                release_id.into(),(*title_key).into(),ids.tvdb.into(),ids.tvmaze.into(),ids.trakt.into(),ids.tmdb.into(),ids.imdb.into(),ids.tvrage.into(),
            ]),
            Self::ReindexWords => (
                "insert into word_queue(release_id) values(?1) on conflict(release_id) do update set n=n+1", vec![release_id.into()],
            ),
            Self::RecordSegmentCrc { message_id, decoded, crc32 } => (
                crate::archive::store::SET_SEGMENT_CRC,
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
            Self::KeepReleaseKeys { keys } => (crate::keys::UPSERT, vec![
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
            Self::KeepProbe { crc32, agent, json, moviehash } => (crate::probe::SET_PROBE,
                vec![release_id.into(),(*crc32).into(),agent.clone().into(),json.clone().into(),moviehash.clone().into()]),
            Self::KeepCover { mime, sha256 } => (crate::probe::SET_COVER,vec![release_id.into(),mime.clone().into(),sha256.clone().into()]),
            Self::ClearCover => (crate::probe::DROP_COVER,vec![release_id.into()]),
            Self::DropNfoCopy { file } => (crate::probe::nfo_store::DROP,vec![release_id.into(),file.clone().into()]),
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
                params.extend(image.sqlite_values());
                ("insert or replace into release_images(release_id,position,file,sha256,format,width,height,bytes,meta,exif_id,document_id)
                  values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params)
            }
            Self::AnswerImageRequest { file, done_at, answer } => (
                "update picture_requests set done_at=?3,outcome=?4 where release_id=?1 and file=?2",
                vec![release_id.into(),file.clone().into(),(*done_at).into(),answer.word().to_string().into()],
            ),
            Self::ClearArchiveSource { source } => (crate::archive::store::DELETE_SOURCE,vec![release_id.into(),(*source).into()]),
            Self::KeepArchiveFile { sequence, source, file } => (crate::archive::store::INSERT_FILE_FACTS,
                vec![release_id.into(),(*sequence).into(),file.name.clone().into(),file.size.and_then(|size| i64::try_from(size).ok()).into(),
                    (*source).into(),file.crc32.map(i64::from).into(),file.mtime.into(),file.host_os.map(i64::from).into()]),
            Self::AdjustArchiveNameBytes { delta } => (crate::archive::store::ADD_NAME_BYTES,vec![(*delta).into()]),
            Self::ArchiveNamesOverQuota => (crate::archive::store::COUNT_OVER_QUOTA,vec![]),
            Self::SetArchivePassword { password } => (crate::archive::store::UPSERT_RELEASE,vec![release_id.into(),password.code().into()]),
            Self::SetArchiveEncryption { encryption, names_hidden, check } => (crate::archive::store::SET_ENCRYPTION,
                vec![release_id.into(),encryption.map(crate::archive::Encryption::code).into(),i64::from(*names_hidden).into(),
                    check.map(|check| check.to_bytes()).into()]),
            Self::SetArchiveKeys { keys, facts } => (crate::archive::store::SET_KEYS,
                vec![release_id.into(),keys.packer.clone().into(),keys.twin_key.into(),keys.owner_key.into(),keys.zone_key.into(),(*facts).into()]),
            Self::ClearRevVolumes => (crate::archive::rev::DELETE_REV_VOLUMES,vec![release_id.into()]),
            Self::KeepRevVolume(volume) => (crate::archive::rev::INSERT_REV_VOLUME,
                vec![release_id.into(),i64::from(volume.index).into(),i64::try_from(volume.size).unwrap_or(i64::MAX).into(),i64::from(volume.crc32).into()]),
            Self::RememberUploadCandidate { candidate_id } => (crate::archive::link::TRIED,vec![release_id.into(),(*candidate_id).into()]),
            Self::PlaceBesideFile { row_id, owner_id, par2_file } => (crate::archive::link::SET_BESIDE,
                vec![(*row_id).into(),(*owner_id).into(),par2_file.clone().into()]),
            Self::NameUploadLead { lead_id, name, source } => (crate::archive::link::NAME_LEAD,
                vec![(*lead_id).into(),name.clone().into(),i64::from(*source).into()]),
            Self::RerunUploadLead { lead_id, clear, net } => (crate::archive::link::RERUN_LEAD,
                vec![(*lead_id).into(),(*clear).into(),(*net).into()]),
            Self::QueueUploadLead { lead_id, due_at } => (crate::archive::link::QUEUE_LEAD,vec![(*lead_id).into(),(*due_at).into()]),
            Self::MoveUpload { from_id, to_id } => (crate::archive::link::MOVE_UPLOAD,vec![(*from_id).into(),(*to_id).into()]),
            Self::LinkUploadData { member_id, lead_id, par2_file } => (crate::archive::link::LINK_DATA,
                vec![(*member_id).into(),(*lead_id).into(),par2_file.to_vec().into()]),
            Self::LinkUploadRecovery { lead_id, wanted, set_id } => (crate::archive::link::LINK_RECOVERY,
                vec![release_id.into(),(*lead_id).into(),(*wanted).into(),set_id.map(|id| id.to_vec()).into()]),
            Self::ClearContentFingerprint => (crate::duplicates::CLEAR_CONTENT,vec![release_id.into()]),
            Self::KeepNfoForLegacyTarget { target_id, file, text } => (
                "insert or ignore into release_nfo(release_id,file,text) values(?1,?2,?3)",
                vec![(*target_id).into(),file.clone().into(),text.chars().take(crate::probe::nfo_store::MAX_CHARS).collect::<String>().into()],
            ),
            Self::MarkPointed { name } => (crate::archive::store::MARK_POINTED,vec![name.clone().into(),release_id.into()]),
            Self::RankPointed { name } => (crate::archive::store::RANK_POINTED.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::RerunPointedList { name } => (crate::archive::store::RERUN_POINTED_LIST.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::RerunPointedProbe { name } => (crate::archive::store::RERUN_POINTED_PROBE.as_str(),vec![name.clone().into(),release_id.into()]),
            Self::SetPointedPassword { name, password } => (crate::archive::store::MARK_POINTED_PASSWORD,vec![name.clone().into(),release_id.into(),password.clone().into()]),
            Self::SetContentFingerprint { content } => (crate::duplicates::SET_CONTENT,vec![release_id.into(),content.clone().into()]),
            Self::SetPostedFingerprint { posted } => (crate::duplicates::SET_POSTED,vec![release_id.into(),posted.clone().into()]),
            Self::ClearPostedFingerprint => (crate::duplicates::CLEAR_POSTED,vec![release_id.into()]),
            Self::SetPar2Sizes { block, blocks } => (crate::duplicates::SET_PAR2_SIZES,vec![release_id.into(),(*block).into(),(*blocks).into()]),
            Self::ClearPar2Files => (crate::archive::store::DELETE_PAR2_FILES,vec![release_id.into()]),
            Self::KeepPar2File { file, sealed, complete } => (crate::archive::store::INSERT_PAR2_FILE,
                vec![release_id.into(),file.file_id.to_vec().into(),file.set_id.to_vec().into(),file.md5.to_vec().into(),file.md5_16k.to_vec().into(),
                    i64::try_from(file.size).unwrap_or(i64::MAX).into(),file.name.clone().into(),file.crc32.map(i64::from).into(),
                    i64::from(file.bound).into(),i64::from(*sealed).into(),i64::from(*complete).into()]),
            Self::SetPar2Creator { set_id, creator } => (crate::archive::store::SET_PAR2_CREATOR,vec![release_id.into(),set_id.to_vec().into(),creator.clone().into()]),
            Self::VouchComplete => (crate::store::SET_VOUCHED_COMPLETE,vec![release_id.into()]),
            Self::DropPcCategory => (crate::junk::DROP_CATEGORY,vec![release_id.into()]),
        }
    }

    #[cfg(test)]
    fn apply_sqlite(&self, conn: &rusqlite::Connection, release_id: i64) -> rusqlite::Result<usize> {
        if matches!(self,Self::ParseMedia {..}|Self::PredbRead {..}|Self::XrefRead {..}|Self::XrefTried {..}) {return Err(rusqlite::Error::InvalidQuery);}
        let (sql, params) = self.sqlite_statement(release_id);
        conn.execute(sql, rusqlite::params_from_iter(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::probe::opensubtitles::OPENSUBTITLES.create).unwrap();
        conn.execute_batch("create table media_release(release_id integer primary key,imdb_id integer,imdb_from_nfo integer);
            insert into media_release values(1,NULL,0),(2,202,0);").unwrap();
        conn
    }

    #[test]
    fn closed_match_mutation_binds_text_and_replaces_a_negative_exactly() {
        let conn = fixture();
        let negative = StepMutation::OpenSubtitlesMatch { moviehash: "hash'quoted".into(), feature: None, checked_at: 1 };
        negative.apply_sqlite(&conn, 1).unwrap();
        let positive = StepMutation::OpenSubtitlesMatch { moviehash: "hash'quoted".into(),
            feature: Some(Feature { imdb_id: Some(101), title: Some("A ' title".into()), ..Default::default() }), checked_at: 2 };
        positive.apply_sqlite(&conn, 1).unwrap();
        let row: (String, Option<i64>, String, i64) = conn.query_row(
            "select moviehash,imdb_id,title,checked_at from release_opensubtitles where release_id=1", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!(row, ("hash'quoted".into(), Some(101), "A ' title".into(), 2));
        negative.apply_sqlite(&conn, 1).unwrap();
        let imdb: Option<i64> = conn.query_row("select imdb_id from release_opensubtitles where release_id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(imdb, None);
    }

    #[test]
    fn closed_imdb_pin_preserves_an_existing_id_and_joins_outer_rollback() {
        let conn = fixture();
        let mutation = StepMutation::PinOpenSubtitlesMovieImdb { imdb_id: 101 };
        conn.execute_batch("begin").unwrap();
        assert_eq!(mutation.apply_sqlite(&conn, 1).unwrap(), 1);
        assert_eq!(mutation.apply_sqlite(&conn, 2).unwrap(), 0);
        conn.execute_batch("rollback").unwrap();
        let rows: Vec<(i64, Option<i64>, i64)> = conn.prepare("select * from media_release order by release_id").unwrap()
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows, vec![(1,None,0),(2,Some(202),0)]);
    }

    #[test]
    fn legacy_or_modified_sql_cannot_be_submitted_as_a_native_mutation() {
        use crate::postprocess::step::StepWrite;
        assert!(StepWrite::new("select 1", vec![]).native_mutation().is_err());
        let mut write = StepWrite::for_mutation(1, StepMutation::PinOpenSubtitlesMovieImdb { imdb_id: 101 });
        assert!(write.native_mutation().is_ok());
        write.sql = "select 1";
        assert!(write.native_mutation().is_err());
    }

    #[test]
    fn closed_category_mutations_replace_and_clear_only_the_target_release() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table release_category(release_id integer primary key,category integer);
            insert into release_category values(1,2040),(2,5040)").unwrap();
        StepMutation::SetCategory { category: 2045 }.apply_sqlite(&conn, 1).unwrap();
        let category: i64 = conn.query_row("select category from release_category where release_id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(category, 2045);
        StepMutation::ClearCategory.apply_sqlite(&conn, 1).unwrap();
        let rows: Vec<(i64, i64)> = conn.prepare("select release_id,category from release_category").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows, vec![(2, 5040)]);
    }

    #[test]
    fn closed_probe_information_preserves_other_probe_fields_and_missing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table release_probe(release_id integer primary key,info text,moviehash text);
            insert into release_probe values(1,'old','retained')").unwrap();
        let mutation = StepMutation::SetProbeInfo { json: "{\"quoted\":\"'\"}".into() };
        assert_eq!(mutation.apply_sqlite(&conn, 1).unwrap(), 1);
        assert_eq!(mutation.apply_sqlite(&conn, 2).unwrap(), 0);
        let row: (String,String) = conn.query_row("select info,moviehash from release_probe", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(row, ("{\"quoted\":\"'\"}".into(), "retained".into()));
    }

    #[test]
    fn closed_sidecar_unread_and_eviction_preserve_names_and_the_current_text() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::build_shard(&conn).unwrap();
        let record = |kind: &str, text: Option<&str>, at| SidecarRecord {
            name: "Example.Release".into(),group: "example.group".into(),file: Some("example.nfo".into()),
            kind: kind.into(),release_name: Some("Example".into()),segment_uid: Some("0011".into()),
            text: text.map(str::to_string),at,
        };
        StepMutation::KeepSidecar(record("nfo",Some("old"),1)).apply_sqlite(&conn,1).unwrap();
        let mut unread = record("unread",None,2);
        unread.file = None; unread.release_name = None; unread.segment_uid = None;
        StepMutation::KeepSidecar(unread).apply_sqlite(&conn,1).unwrap();
        let row: (String,String,String,String,String) = conn.query_row(
            "select file,kind,release_name,segment_uid,text from sidecars where release_id=1", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap();
        assert_eq!(row,("example.nfo".into(),"nfo".into(),"Example".into(),"0011".into(),"old".into()));
        StepMutation::KeepSidecar(record("nfo",Some("new!"),3)).apply_sqlite(&conn,2).unwrap();
        StepMutation::EvictSidecarTexts { max_bytes: 4 }.apply_sqlite(&conn,2).unwrap();
        let rows: Vec<(i64,Option<String>,String)> = conn.prepare("select release_id,text,release_name from sidecars order by release_id").unwrap()
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows,vec![(1,None,"Example".into()),(2,Some("new!".into()),"Example".into())]);
        assert_eq!(crate::sidecar::text_bytes(&conn),4);
    }

    #[test]
    fn closed_nfo_mutations_keep_first_text_character_cap_and_empty_password() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::probe::nfo_store::RELEASE_NFO.create).unwrap();
        conn.execute_batch(crate::archive::store::NFO_PASSWORD.create).unwrap();
        let text = "é".repeat(crate::probe::nfo_store::MAX_CHARS+1);
        StepMutation::KeepNfo { file: "first.nfo".into(),text }.apply_sqlite(&conn,1).unwrap();
        StepMutation::KeepNfo { file: "second.nfo".into(),text: "replacement".into() }.apply_sqlite(&conn,1).unwrap();
        let (file,text): (String,String) = conn.query_row("select file,text from release_nfo", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(file,"first.nfo");
        assert_eq!(text.chars().count(),crate::probe::nfo_store::MAX_CHARS);
        StepMutation::SetNfoPassword { password: String::new() }.apply_sqlite(&conn,1).unwrap();
        let password: Option<String> = conn.query_row("select password from release_nfo_password", [], |row| row.get(0)).unwrap();
        assert_eq!(password,Some(String::new()));
    }

    #[test]
    fn closed_media_ids_require_the_same_title_and_preserve_pinned_facts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::media::MEDIA_RELEASE.create).unwrap();
        for (column, declaration) in crate::media::MEDIA_RELEASE.added {
            conn.execute_batch(&format!("alter table media_release add column {column} {declaration}")).unwrap();
        }
        conn.execute_batch("insert into media_release(release_id,kind,title_key,quality,tvdb_id,tmdb_id,imdb_id,tvdb_from_tag,tmdb_from_tag,imdb_from_nfo)
            values(1,1,77,0,111,222,333,1,1,1)").unwrap();
        let ids = crate::media::resolve::Ids { tvdb: Some(1),tmdb: Some(2),imdb: Some(3),tvmaze: Some(444),..Default::default() };
        assert_eq!(StepMutation::FoundMediaIds { title_key: 88,ids }.apply_sqlite(&conn,1).unwrap(),0);
        assert_eq!(StepMutation::FoundMediaIds { title_key: 77,ids }.apply_sqlite(&conn,1).unwrap(),1);
        let kept: (i64,i64,i64,i64) = conn.query_row("select tvdb_id,tmdb_id,imdb_id,tvmaze_id from media_release", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!(kept,(111,222,333,444));
        StepMutation::ClearMedia.apply_sqlite(&conn,2).unwrap();
        let count: i64 = conn.query_row("select count(*) from media_release", [], |row| row.get(0)).unwrap();
        assert_eq!(count,1);
    }

    #[test]
    fn closed_crc_and_word_queue_keep_exact_keys_unsigned_crc_and_repeat_counts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::archive::store::SEGMENT_CRC.create).unwrap();
        conn.execute_batch("create table word_queue(release_id integer primary key,n integer not null default 0)").unwrap();
        StepMutation::RecordSegmentCrc { message_id: "<exact'ID@example>".into(),decoded: 512,crc32: u32::MAX }.apply_sqlite(&conn,1).unwrap();
        let row: (String,i64,i64) = conn.query_row("select message_id,decoded,crc32 from segment_crc", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(row,("<exact'ID@example>".into(),512,4_294_967_295));
        StepMutation::ReindexWords.apply_sqlite(&conn,1).unwrap();
        StepMutation::ReindexWords.apply_sqlite(&conn,1).unwrap();
        let count: i64 = conn.query_row("select n from word_queue where release_id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(count,1);
    }

    #[test]
    fn closed_predb_match_and_inner_keys_preserve_nullable_facts_and_target_scope() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::predb::step::MATCH.create).unwrap();
        conn.execute_batch(crate::predb::step::INNER_KEY.create).unwrap();
        StepMutation::KeepPredbMatch { title_id: 9,kind: crate::predb::matcher::Kind::InnerFile,
            category: None,pre_at: None,nuked: 1,matched_at: 100 }.apply_sqlite(&conn,1).unwrap();
        let row: (i64,i64,Option<String>,Option<i64>,i64,i64) = conn.query_row(
            "select title_id,kind,category,pre_at,nuked,matched_at from predb_match", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap();
        assert_eq!(row,(9,4,None,None,1,100));
        for id in [1,1,2] {
            StepMutation::AddPredbInnerKey { key: -7 }.apply_sqlite(&conn,id).unwrap();
        }
        StepMutation::ClearPredbInnerKeys.apply_sqlite(&conn,1).unwrap();
        let rows: Vec<(i64,i64)> = conn.prepare("select key,release_id from predb_inner_key").unwrap()
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows,vec![(-7,2)]);
    }

    #[test]
    fn closed_release_keys_preserve_signed_bits_nulls_and_targeted_clear() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::build_shard(&conn).unwrap();
        let keys = crate::keys::Keys { account: Some(i64::MIN),file_crc: Some(-7),volumes: Some(i64::MAX),..Default::default() };
        StepMutation::KeepReleaseKeys { keys }.apply_sqlite(&conn,1).unwrap();
        StepMutation::KeepReleaseKeys { keys }.apply_sqlite(&conn,2).unwrap();
        assert_eq!(crate::keys::stored(&conn,1).unwrap(),Some(keys));
        StepMutation::ClearReleaseKeys.apply_sqlite(&conn,1).unwrap();
        assert_eq!(crate::keys::stored(&conn,1).unwrap(),None);
        assert_eq!(crate::keys::stored(&conn,2).unwrap(),Some(keys));
    }

    #[test]
    fn closed_still_and_audio_results_keep_nullable_time_hash_bits_and_blob_items() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::probe::stills::STILLS.create).unwrap();
        conn.execute_batch("alter table release_stills add column phash integer").unwrap();
        conn.execute_batch(crate::probe::audio_print::AUDIO_PRINTS.create).unwrap();
        conn.execute_batch(crate::probe::audio_print::AUDIO_ANCHORS.create).unwrap();
        StepMutation::KeepStill(crate::raw_store::SavedStill { position: 25,at_ms: None,sha256: "a".repeat(64),
            width: 960,height: 540,phash: Some(i64::MIN) }).apply_sqlite(&conn,1).unwrap();
        let still: (Option<i64>,i64,i64,i64) = conn.query_row("select at_ms,width,height,phash from release_stills", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!(still,(None,960,540,i64::MIN));
        StepMutation::KeepAudioPrint { position: 50,at_ms: 1000,items: vec![0,u32::MAX] }.apply_sqlite(&conn,1).unwrap();
        let blob: Vec<u8> = conn.query_row("select items from release_audio_print", [], |row| row.get(0)).unwrap();
        assert_eq!(crate::audio::from_blob(&blob),vec![0,u32::MAX]);
        for id in [1,1,2] {
            StepMutation::KeepAudioAnchor { anchor: i64::MIN }.apply_sqlite(&conn,id).unwrap();
        }
        StepMutation::ClearAudioPrints.apply_sqlite(&conn,1).unwrap();
        StepMutation::ClearAudioAnchors.apply_sqlite(&conn,1).unwrap();
        StepMutation::ClearStills.apply_sqlite(&conn,2).unwrap();
        let retained: Vec<i64> = conn.prepare("select release_id from release_audio_anchor").unwrap()
            .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(retained,vec![2]);
        assert_eq!(conn.query_row("select count(*) from release_stills", [], |row| row.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(conn.query_row("select count(*) from release_audio_print", [], |row| row.get::<_,i64>(0)).unwrap(),0);
    }

    #[test]
    fn closed_probe_cover_and_nfo_copy_results_preserve_nulls_and_exact_targets() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::probe::PROBE.create).unwrap();
        for (column, declaration) in crate::probe::PROBE.added {
            conn.execute_batch(&format!("alter table release_probe add column {column} {declaration}")).unwrap();
        }
        conn.execute_batch(crate::probe::COVER.create).unwrap();
        conn.execute_batch(crate::probe::nfo_store::RELEASE_NFO.create).unwrap();
        let json = "{\"nfo\":\"quoted ' text\"}";
        StepMutation::KeepProbe { crc32: None,agent: None,json: json.into(),moviehash: None }.apply_sqlite(&conn,1).unwrap();
        let row: (Option<i64>,Option<String>,String,Option<String>) = conn.query_row(
            "select crc32,agent,info,moviehash from release_probe", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!(row,(None,None,json.into(),None));
        StepMutation::KeepCover { mime: "image/jpeg".into(),sha256: "a".repeat(64) }.apply_sqlite(&conn,1).unwrap();
        StepMutation::ClearCover.apply_sqlite(&conn,2).unwrap();
        assert_eq!(conn.query_row("select count(*) from release_cover", [], |row| row.get::<_,i64>(0)).unwrap(),1);
        StepMutation::KeepNfo { file: "first.nfo".into(),text: "retained".into() }.apply_sqlite(&conn,1).unwrap();
        assert_eq!(StepMutation::DropNfoCopy { file: "other.nfo".into() }.apply_sqlite(&conn,1).unwrap(),0);
        assert_eq!(StepMutation::DropNfoCopy { file: "first.nfo".into() }.apply_sqlite(&conn,1).unwrap(),1);
    }

    #[test]
    fn closed_image_result_and_answer_keep_metadata_and_exact_request_target() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::images::step::IMAGES.create).unwrap();
        conn.execute_batch(crate::images::step::PICTURE_REQUESTS.create).unwrap();
        conn.execute_batch("insert into picture_requests(release_id,file,requested_at) values(1,'image.jpg',1),(2,'image.jpg',1)").unwrap();
        StepMutation::KeepImage { position: 0,image: ImageRecord { file: "image.jpg".into(),sha256: "a".repeat(64),
            format: "jpeg".into(),width: 960,height: 540,bytes: 123,meta: Some("{\"quoted\":\"'\"}".into()),exif_id: None,document_id: None } }
            .apply_sqlite(&conn,1).unwrap();
        let row: (String,i64,i64,Option<String>,Option<String>) = conn.query_row(
            "select file,width,height,meta,exif_id from release_images", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap();
        assert_eq!(row,("image.jpg".into(),960,540,Some("{\"quoted\":\"'\"}".into()),None));
        StepMutation::AnswerImageRequest { file: "image.jpg".into(),done_at: 2,answer: crate::images::step::Answer::NotAPicture }.apply_sqlite(&conn,1).unwrap();
        let answers: Vec<(i64,Option<i64>,Option<String>)> = conn.prepare("select release_id,done_at,outcome from picture_requests order by release_id").unwrap()
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(answers,vec![(1,Some(2),Some("not a picture".into())),(2,None,None)]);
        StepMutation::ClearImages.apply_sqlite(&conn,2).unwrap();
        assert_eq!(conn.query_row("select count(*) from release_images", [], |row| row.get::<_,i64>(0)).unwrap(),1);
    }

    #[test]
    fn closed_archive_results_preserve_header_facts_quota_and_unsigned_volume_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table releases(id integer primary key);create table meta(key text primary key,value integer)").unwrap();
        for table in [&crate::archive::store::INNER_FILES,&crate::archive::store::RELEASE,&crate::archive::rev::REV_VOLUMES] {
            conn.execute_batch(table.create).unwrap();
            for (column, declaration) in table.added {
                conn.execute_batch(&format!("alter table {} add column {column} {declaration}",table.name)).unwrap();
            }
        }
        StepMutation::KeepArchiveFile { sequence: 131072,source: 2,file: crate::archive::InnerFile {
            name: "é".into(),size: Some(u64::MAX),crc32: Some(u32::MAX),mtime: Some(42),host_os: Some(3),
        } }.apply_sqlite(&conn,1).unwrap();
        let row: (Option<i64>,i64,i64,i64) = conn.query_row("select size,crc32,mtime,host_os from archive_inner_files", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!(row,(None,4_294_967_295,42,3));
        StepMutation::AdjustArchiveNameBytes { delta: 34 }.apply_sqlite(&conn,1).unwrap();
        StepMutation::ArchiveNamesOverQuota.apply_sqlite(&conn,1).unwrap();
        StepMutation::SetArchivePassword { password: crate::archive::Password::Maybe }.apply_sqlite(&conn,1).unwrap();
        StepMutation::SetArchiveEncryption { encryption: Some(crate::archive::Encryption::ZipAes),names_hidden: true,check: None }.apply_sqlite(&conn,1).unwrap();
        StepMutation::SetArchiveKeys { keys: crate::archive::store::ArchiveKeys { owner_key: Some(i64::MIN),..Default::default() },facts: 3 }.apply_sqlite(&conn,1).unwrap();
        let state: (i64,i64,i64,i64,i64) = conn.query_row("select password,encryption,names_hidden,facts,owner_key from archive_release", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap();
        assert_eq!(state,(2,5,1,3,i64::MIN));
        assert_eq!(crate::store::get_meta(&conn,"archive_name_bytes").unwrap(),Some(34));
        assert_eq!(crate::store::get_meta(&conn,"archive_names_over_quota").unwrap(),Some(1));
        StepMutation::KeepRevVolume(crate::archive::rev::Volume { index: u16::MAX,size: u64::MAX,crc32: u32::MAX }).apply_sqlite(&conn,1).unwrap();
        let volume: (i64,i64,i64) = conn.query_row("select volume,size,crc32 from rev_volumes", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(volume,(65_535,i64::MAX,4_294_967_295));
    }

    #[test]
    fn closed_upload_results_follow_current_lead_and_require_native_target_resolution() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::archive::link::UPLOAD_LINK.create).unwrap();
        conn.execute_batch(crate::archive::link::UPLOAD_LINK_TRIED.create).unwrap();
        conn.execute_batch(crate::archive::link::BESIDE_FILES.create).unwrap();
        conn.execute_batch("insert into upload_link(release_id,lead_id) values(2,2),(3,3)").unwrap();
        StepMutation::LinkUploadData { member_id: 4,lead_id: 2,par2_file: [1;16] }.apply_sqlite(&conn,1).unwrap();
        StepMutation::MoveUpload { from_id: 2,to_id: 3 }.apply_sqlite(&conn,1).unwrap();
        StepMutation::LinkUploadRecovery { lead_id: 2,wanted: 5,set_id: Some([2;16]) }.apply_sqlite(&conn,1).unwrap();
        let recovery: (i64,i64,Vec<u8>) = conn.query_row("select lead_id,wanted,set_id from upload_link where release_id=1", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(recovery,(3,5,vec![2;16]));
        StepMutation::RememberUploadCandidate { candidate_id: 4 }.apply_sqlite(&conn,1).unwrap();
        StepMutation::RememberUploadCandidate { candidate_id: 4 }.apply_sqlite(&conn,1).unwrap();
        assert_eq!(conn.query_row("select count(*) from upload_link_tried", [], |row| row.get::<_,i64>(0)).unwrap(),1);
        StepMutation::PlaceBesideFile { row_id: 5,owner_id: 1,par2_file: None }.apply_sqlite(&conn,1).unwrap();
        let owner: i64 = conn.query_row("select release_id from beside_files where row_id=5", [], |row| row.get(0)).unwrap();
        assert_eq!(owner,1);
        let write = crate::postprocess::step::StepWrite::for_mutation(1,
            StepMutation::LinkUploadData { member_id: 4,lead_id: 2,par2_file: [1;16] });
        assert!(matches!(write.native_mutation(),Err(crate::postprocess::step::StepError::Retry(_))));
    }

    #[test]
    fn a_beside_nfo_result_keeps_its_legacy_owner_without_minting_native_authority() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::probe::nfo_store::RELEASE_NFO.create).unwrap();
        let mutation = StepMutation::KeepNfoForLegacyTarget { target_id: 2,file: "example.nfo".into(),text: "retained".into() };
        mutation.apply_sqlite(&conn,1).unwrap();
        let owner: i64 = conn.query_row("select release_id from release_nfo", [], |row| row.get(0)).unwrap();
        assert_eq!(owner,2);
        let write = crate::postprocess::step::StepWrite::for_mutation(1,mutation);
        assert!(matches!(write.native_mutation(),Err(crate::postprocess::step::StepError::Retry(_))));
    }

    #[test]
    fn pointed_name_result_keeps_legacy_parameter_order_and_requires_the_resolver() {
        let write = crate::postprocess::step::StepWrite::for_mutation(7,StepMutation::MarkPointed { name: "Example.Release".into() });
        assert_eq!(write.sql,crate::archive::store::MARK_POINTED);
        assert_eq!(write.params,vec!["Example.Release".to_string().into(),7i64.into()]);
        assert!(matches!(write.native_mutation(),Err(crate::postprocess::step::StepError::Retry(_))));
    }

    #[test]
    fn closed_par2_and_fingerprint_results_keep_sealing_nullable_crc_and_independent_facts() {
        let conn = Connection::open_in_memory().unwrap();
        for table in [&crate::archive::store::PAR2_FILES,&crate::duplicates::FINGERPRINT] {
            conn.execute_batch(table.create).unwrap();
            for (column, declaration) in table.added {
                conn.execute_batch(&format!("alter table {} add column {column} {declaration}",table.name)).unwrap();
            }
        }
        let file = crate::par2::Par2File { set_id: [1;16],file_id: [2;16],md5: [3;16],md5_16k: [4;16],
            size: u64::MAX,name: "example'file.mkv".into(),bound: false,crc32: None };
        let mut inventory = crate::par2::Inventory { files: vec![file.clone()],..Default::default() };
        inventory.sealed.push((file.set_id,file.file_id));
        inventory.creators.push((file.set_id,"synthetic'creator".into()));
        for write in crate::archive::store::par2_file_writes(1,&inventory) {
            conn.execute(write.sql,rusqlite::params_from_iter(write.params)).unwrap();
        }
        let row: (i64,Option<i64>,i64,i64,i64,String) = conn.query_row(
            "select size,crc32,bound,sealed,complete,creator from par2_files", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap();
        assert_eq!(row,(i64::MAX,None,0,0,0,"synthetic'creator".into()));
        StepMutation::SetContentFingerprint { content: vec![0,255] }.apply_sqlite(&conn,1).unwrap();
        StepMutation::SetPostedFingerprint { posted: vec![255,0] }.apply_sqlite(&conn,1).unwrap();
        StepMutation::SetPar2Sizes { block: 512,blocks: 7 }.apply_sqlite(&conn,1).unwrap();
        StepMutation::ClearPostedFingerprint.apply_sqlite(&conn,1).unwrap();
        let fingerprints: (Option<Vec<u8>>,Option<Vec<u8>>,Option<i64>) = conn.query_row(
            "select content,posted,par2_block from release_fingerprint", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(fingerprints,(Some(vec![0,255]),None,None));
    }

    #[test]
    fn deferred_media_parse_stays_legacy_and_keeps_writer_time_values() {
        let home = tempfile::tempdir().unwrap();
        let media = crate::media::Media::new(&home.path().join("atlas.db"));
        let parsed = crate::media::parse::Parsed { title: "Example".into(),year: Some(2020),season: Some(2),
            episode: Some(3),episode_last: Some(4),air_date: Some(20000101),quality: 1080,..Default::default() };
        let mut write = crate::postprocess::step::StepWrite::for_deferred_legacy(7,DeferredLegacyStepMutation::MediaParse {
            media,kind: crate::media::title::Kind::Tv,title_key: 77,parsed,
        });
        assert_eq!(write.sql,crate::media::steps::UPSERT);
        assert!(write.params.is_empty());
        assert!(matches!(write.native_mutation(),Err(crate::postprocess::step::StepError::Retry(_))));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(),0);
        let params = write.late_params.take().unwrap()();
        assert_eq!(&params[..9],&[7i64.into(),1i64.into(),77i64.into(),2i64.into(),3i64.into(),4i64.into(),20000101i64.into(),2020i64.into(),1080i64.into()]);
        assert_eq!(&params[9..],&[Value::Null,Value::Null,Value::Null,Value::Null,Value::Null,Value::Null]);
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(),0);
    }
}
