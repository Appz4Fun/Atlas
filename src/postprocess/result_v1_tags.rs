//! Frozen retained v1 digest atoms. These are not executable SQL or caller input.
//! Exact atom/field bytes remain stable while storage implementations retire.

macro_rules! lead_now {
    ($lead:literal) => {
        concat!("coalesce((select n.lead_id from upload_link n where n.release_id = ", $lead, "), ", $lead, ")")
    };
}

macro_rules! set_vouches_sql {
    () => {
        "exists (select 1 from par2_files where release_id = ?1 and complete = 1)
         and not exists (select 1 from par2_files p where p.release_id = ?1 and p.complete = 1
             and not exists (select 1 from files f where f.release_id = ?1 and f.filename = p.name))
         and not exists (select 1 from files f where f.release_id = ?1
             and lower(f.filename) not like '%.par2' and lower(f.filename) not like '%.nfo'
             and lower(f.filename) not like '%.sfv' and lower(f.filename) not like '%.srr'
             and lower(f.filename) not like '%.srs' and lower(f.filename) not like '%.txt'
             and lower(f.filename) not like '%.jpg' and lower(f.filename) not like '%.png'
             and lower(f.filename) not like '%.md5'
             and not exists (select 1 from par2_files p where p.release_id = ?1 and p.complete = 1
                 and p.name = f.filename))"
    };
}

const NET_RANK: &str = "coalesce((select max(case
        when c.category is not null and c.category not between 2000 and 2999 and c.category not between 5000 and 5999
            and c.category not between 8000 and 8999 then 0
        when r.size >= 42949672960 then 4
        when r.size >= 16106127360 then 3
        when r.size >= 4294967296 then 2
        when r.size >= 1073741824 then 1
        when r.size >= 52428800 and (c.category between 2000 and 2999 or c.category between 5000 and 5999) then 1
        else 0 end,
        case
            when r.size <= 67108864 and exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')
                then 5
            when r.size <= 4194304 and exists (select 1 from files f where f.release_id = r.id)
                and not exists (select 1 from files f where f.release_id = r.id and not (
                    f.filename like '%.nfo' or f.filename like '%.txt' or f.filename like '%.sfv'
                    or f.filename like '%.srr' or f.filename like '%.srs' or f.filename like '%.md5'
                    or f.filename like '%.sha1' or f.filename like '%.diz'
                    or f.filename like '%.jpg' or f.filename like '%.jpeg' or f.filename like '%.png'
                    or f.filename like '%.gif' or f.filename like '%.webp'))
                and ((select count(*) from files f where f.release_id = r.id) = 1
                    or exists (select 1 from files f where f.release_id = r.id and not (
                        f.filename like '%.jpg' or f.filename like '%.jpeg' or f.filename like '%.png'
                        or f.filename like '%.gif' or f.filename like '%.webp')))
                then 5
            when exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')
                or exists (select 1 from nzb_pointed p where p.release_id = r.id) then 3
            else 0 end)
    from releases r left join release_category c on c.release_id = r.id where r.id = ?1), 0)";

pub(super) const ARCHIVE_LINK_LINK_DATA: &str = concat!(
    "insert into upload_link (release_id, lead_id, recovery, par2_file) values (?1, ",
    lead_now!("?2"),
    ", 0, ?3)
    on conflict(release_id) do update set lead_id = excluded.lead_id, recovery = 0, par2_file = excluded.par2_file"
);

pub(super) const ARCHIVE_LINK_LINK_RECOVERY: &str = concat!(
    "insert into upload_link (release_id, lead_id, recovery, wanted, set_id) values (?1, ",
    lead_now!("?2"),
    ", 1, ?3, ?4)
    on conflict(release_id) do update set lead_id = excluded.lead_id, recovery = 1, wanted = excluded.wanted,
        set_id = excluded.set_id"
);

pub(super) const ARCHIVE_LINK_MOVE_UPLOAD: &str = concat!("update upload_link set lead_id = ", lead_now!("?2"), " where lead_id = ?1 and ?1 != ?2");

pub(super) const ARCHIVE_LINK_NAME_LEAD: &str = "update releases set display_name = ?2, name_source = ?3
    where id = ?1 and display_name is null and coalesce(name_source, 0) <= ?3";

pub(super) const ARCHIVE_LINK_QUEUE_LEAD: &str = "insert into pp_queue (release_id, due_at) values (?1, ?2)
    on conflict(release_id) do update set due_at = min(due_at, excluded.due_at)";

pub(super) const ARCHIVE_LINK_RERUN_LEAD: &str = "insert into pp_state (release_id, net_wanted, rerun_gen, rerun_mask, net_left)
        values (?1, case when ?3 != 0 then 1 end, 1, ?2, nullif(?3, 0))
    on conflict(release_id) do update set done = done & ~?2, failed = failed & ~?2,
        net_wanted = case when ?3 != 0 then 1 else net_wanted end,
        net_left = case when ?3 != 0 then coalesce(net_left, 0) | ?3 else net_left end,
        net_next_at = case when ?3 != 0 then 0 else net_next_at end,
        rerun_gen = rerun_gen + 1, rerun_mask = rerun_mask | ?2";

pub(super) const ARCHIVE_LINK_SET_BESIDE: &str = "insert into beside_files (row_id, release_id, par2_file) values (?1, ?2, ?3)
    on conflict(row_id) do update set release_id = excluded.release_id, par2_file = excluded.par2_file";

pub(super) const ARCHIVE_LINK_TRIED: &str = "insert into upload_link_tried (release_id, candidate_id) values (?1, ?2) on conflict do nothing";

pub(super) const ARCHIVE_REV_DELETE_REV_VOLUMES: &str = "delete from rev_volumes where release_id = ?1";

pub(super) const ARCHIVE_REV_INSERT_REV_VOLUME: &str = "insert or replace into rev_volumes (release_id, volume, size, crc32) values (?1, ?2, ?3, ?4)";

pub(super) const ARCHIVE_STORE_ADD_NAME_BYTES: &str = "insert into meta (key, value) values ('archive_name_bytes', ?1)
    on conflict(key) do update set value = value + excluded.value";

pub(super) const ARCHIVE_STORE_COUNT_OVER_QUOTA: &str = "insert into meta (key, value) values ('archive_names_over_quota', 1)
    on conflict(key) do update set value = value + 1";

pub(super) const ARCHIVE_STORE_DELETE_PAR2_FILES: &str = "delete from par2_files where release_id = ?1";

pub(super) const ARCHIVE_STORE_DELETE_SOURCE: &str = "delete from archive_inner_files where release_id = ?1 and source = ?2";

pub(super) const ARCHIVE_STORE_INSERT_FILE_FACTS: &str = "insert or replace into archive_inner_files
    (release_id, seq, name, size, source, crc32, mtime, host_os) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

pub(super) const ARCHIVE_STORE_INSERT_PAR2_FILE: &str = "insert or replace into par2_files
    (release_id, file_id, set_id, md5, md5_16k, size, name, crc32, bound, sealed, complete)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";

pub(super) const ARCHIVE_STORE_MARK_POINTED: &str = "insert or ignore into nzb_pointed (release_id) select id from releases where name = ?1 and id != ?2";

pub(super) const ARCHIVE_STORE_MARK_POINTED_PASSWORD: &str = "insert or ignore into release_nfo_password (release_id, password) select id, ?3 from releases where name = ?1 and id != ?2";

pub(super) static ARCHIVE_STORE_RANK_POINTED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "update pp_state set net_rank = {}
         where net_wanted = 1 and release_id in (select id from releases where name = ?1 and id != ?2)",
        NET_RANK.replace("?1", "pp_state.release_id")
    )
});

pub(super) static ARCHIVE_STORE_RERUN_POINTED_LIST: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let net_clear = 4i64
        | 16i64;
    let all_clear = net_clear | 1099511627776i64;
    format!(
        "update pp_state set
            done = done & ~{all_clear},
            failed = failed & ~{all_clear},
            net_wanted = 1,
            net_left = coalesce(net_left, 0) | {net_clear},
            net_next_at = 0,
            net_attempts = 0,
            net_transient = 0,
            rerun_gen = rerun_gen + 1,
            rerun_mask = rerun_mask | {all_clear}
         where release_id in (select id from releases where name = ?1 and id != ?2)
           and not exists (select 1 from release_nfo_password p where p.release_id = pp_state.release_id)
           and exists (
               select 1 from archive_release ar
               where ar.release_id = pp_state.release_id
                 and (
                     (ar.names_hidden = 1
                      and not exists (
                          select 1 from archive_inner_files f
                          where f.release_id = ar.release_id and f.source in (1, 2, 3)
                      ))
                     or (ar.password = 1
                         and exists (
                             select 1 from archive_inner_files f
                             where f.release_id = ar.release_id and f.source in (1, 2, 3) and lower(f.name) like '%.nzb'
                         )
                         and not exists (
                             select 1 from archive_inner_files f
                             where f.release_id = ar.release_id and f.source = 5
                         ))
                 )
           )"
    )
});

pub(super) static ARCHIVE_STORE_RERUN_POINTED_PROBE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let net_clear = 16i64;
    let all_clear = net_clear | 1099511627776i64;
    format!(
        "update pp_state set
            done = done & ~{all_clear},
            failed = failed & ~{all_clear},
            net_wanted = 1,
            net_left = coalesce(net_left, 0) | {net_clear},
            net_next_at = 0,
            net_attempts = 0,
            net_transient = 0,
            rerun_gen = rerun_gen + 1,
            rerun_mask = rerun_mask | {all_clear}
         where release_id in (select id from releases where name = ?1 and id != ?2)
           and not exists (select 1 from release_nfo_password p where p.release_id = pp_state.release_id)
           and exists (
               select 1 from release_probe rp
               where rp.release_id = pp_state.release_id and rp.info like '%\"encrypted\":true%'
           )"
    )
});

pub(super) const ARCHIVE_STORE_SET_ENCRYPTION: &str = "update archive_release set encryption = ?2, names_hidden = ?3, password_check = ?4 where release_id = ?1";

pub(super) const ARCHIVE_STORE_SET_KEYS: &str = "update archive_release set packer = ?2, twin_key = ?3, owner_key = ?4, zone_key = ?5,
    facts = ?6 where release_id = ?1";

pub(super) const ARCHIVE_STORE_SET_NFO_PASSWORD: &str = "insert or replace into release_nfo_password (release_id, password) values (?1, ?2)";

pub(super) const ARCHIVE_STORE_SET_PAR2_CREATOR: &str = "update par2_files set creator = ?3 where release_id = ?1 and set_id = ?2";

pub(super) const ARCHIVE_STORE_SET_SEGMENT_CRC: &str = "insert or replace into segment_crc (release_id, message_id, decoded, crc32) values (?1, ?2, ?3, ?4)";

pub(super) const ARCHIVE_STORE_UPSERT_RELEASE: &str = "insert into archive_release (release_id, password) values (?1, ?2)
    on conflict(release_id) do update set password = excluded.password";

pub(super) const DUPLICATES_CLEAR_CONTENT: &str = "update release_fingerprint set content = null where release_id = ?1";

pub(super) const DUPLICATES_CLEAR_POSTED: &str = "update release_fingerprint set posted = null, par2_block = null,
    par2_blocks = null where release_id = ?1";

pub(super) const DUPLICATES_SET_CONTENT: &str = "insert into release_fingerprint (release_id, content) values (?1, ?2)
    on conflict(release_id) do update set content = excluded.content";

pub(super) const DUPLICATES_SET_PAR2_SIZES: &str = "insert into release_fingerprint (release_id, par2_block, par2_blocks) values (?1, ?2, ?3)
    on conflict(release_id) do update set par2_block = excluded.par2_block, par2_blocks = excluded.par2_blocks";

pub(super) const DUPLICATES_SET_POSTED: &str = "insert into release_fingerprint (release_id, posted) values (?1, ?2)
    on conflict(release_id) do update set posted = excluded.posted";

pub(super) const JUNK_DROP_CATEGORY: &str = "delete from release_category where release_id = ?1";

pub(super) const KEYS_UPSERT: &str = "insert or replace into release_keys
    (release_id, account, host, layout, lines, file_crc, body_crc, video, audio, mux, encode, thread, subs, fonts,
     chapters, volumes)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)";

pub(super) const MEDIA_STEPS_FOUND: &str = "update media_release
    set tvdb_id = iif(tvdb_from_tag = 1, tvdb_id, ?3), tvmaze_id = ?4, trakt_id = ?5, tmdb_id = iif(tmdb_from_tag = 1, tmdb_id, ?6),
        imdb_id = iif(imdb_from_nfo = 1, imdb_id, ?7), tvrage_id = ?8
    where release_id = ?1 and title_key = ?2";

pub(super) const MEDIA_STEPS_UPSERT: &str = "insert into media_release (release_id, kind, title_key, season, episode, episode_last, air_date, year, quality,
                           tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id)
values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
on conflict(release_id) do update set
    tvdb_id   = iif(media_release.title_key = excluded.title_key, iif(media_release.tvdb_from_tag = 1, media_release.tvdb_id, coalesce(excluded.tvdb_id, media_release.tvdb_id)), excluded.tvdb_id),
    tvmaze_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvmaze_id, media_release.tvmaze_id), excluded.tvmaze_id),
    trakt_id  = iif(media_release.title_key = excluded.title_key, coalesce(excluded.trakt_id,  media_release.trakt_id),  excluded.trakt_id),
    tmdb_id   = iif(media_release.title_key = excluded.title_key, iif(media_release.tmdb_from_tag = 1, media_release.tmdb_id, coalesce(excluded.tmdb_id, media_release.tmdb_id)), excluded.tmdb_id),
    tvrage_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvrage_id, media_release.tvrage_id), excluded.tvrage_id),
    imdb_id   = iif(media_release.title_key = excluded.title_key, iif(media_release.imdb_from_nfo = 1, media_release.imdb_id, coalesce(excluded.imdb_id, media_release.imdb_id)), excluded.imdb_id),
    imdb_from_nfo = iif(media_release.title_key = excluded.title_key, media_release.imdb_from_nfo, 0),
    tmdb_from_tag = iif(media_release.title_key = excluded.title_key, media_release.tmdb_from_tag, 0),
    tvdb_from_tag = iif(media_release.title_key = excluded.title_key, media_release.tvdb_from_tag, 0),
    kind = excluded.kind, title_key = excluded.title_key, season = excluded.season, episode = excluded.episode,
    episode_last = excluded.episode_last, air_date = excluded.air_date, year = excluded.year, quality = excluded.quality";

pub(super) const PROBE_DROP_COVER: &str = "delete from release_cover where release_id = ?1";

pub(super) const PROBE_SET_COVER: &str = "insert or replace into release_cover (release_id, mime, sha256) values (?1, ?2, ?3)";

pub(super) const PROBE_SET_PROBE: &str = "insert or replace into release_probe (release_id, crc32, agent, info, moviehash)
    values (?1, ?2, ?3, ?4, ?5)";

pub(super) const PROBE_NFO_STORE_DROP: &str = "delete from release_nfo where release_id = ?1 and file = ?2";

pub(super) const PROBE_NFO_STORE_MAX_CHARS: usize = 256 * 1024;

pub(super) const SIDECAR_EVICT: &str = "update sidecars set text = null where release_id in (
    select release_id from (
        select release_id, length(cast(text as blob)) as len,
            sum(length(cast(text as blob))) over (order by at, release_id rows unbounded preceding) as run
        from sidecars indexed by sidecars_text_at where text is not null and release_id != ?1)
    where run - len < coalesce((select value from meta where key = 'sidecar_text_bytes'), 0) - ?2)";

pub(super) const SIDECAR_SET: &str = "insert into sidecars
    (release_id, name, group_name, file, kind, release_name, segment_uid, text, at)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    on conflict(release_id) do update set name = excluded.name, group_name = excluded.group_name,
        file = case when excluded.kind = 'unread' then sidecars.file else excluded.file end,
        kind = case when excluded.kind = 'unread' then sidecars.kind else excluded.kind end,
        release_name = case when excluded.kind = 'unread' then sidecars.release_name else excluded.release_name end,
        segment_uid = case when excluded.kind = 'unread' then sidecars.segment_uid else excluded.segment_uid end,
        text = coalesce(excluded.text, sidecars.text), at = excluded.at";

pub(super) const STORE_SET_VOUCHED_COMPLETE: &str = concat!(
    "update releases set complete = 1 where id = ?1 and complete = 0
     and exists (select 1 from files where release_id = ?1)
     and not exists (select 1 from files where release_id = ?1 and not atlas_file_whole(seen, expected))
     and ",
    set_vouches_sql!()
);

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest,Sha256};
    #[test]
    fn every_frozen_v1_atom_matches_its_pre_removal_bytes() {
        let atoms: &[(&str,usize,&str)] = &[

            (&ARCHIVE_LINK_LINK_DATA,977,"f225652029be317f8dc2e7b3907aeccf209c1ed7419fd6b00bb3c60002257738"),

            (&ARCHIVE_LINK_LINK_RECOVERY,982,"e211310ca6f303b817e4340133d4d6b19a549aac0a4803b966e254f703a8df5d"),

            (&ARCHIVE_LINK_MOVE_UPLOAD,931,"bdd9b0e39dae494c208fdf204b62b609036b93f72ca43d3ea37de0594cdaf105"),

            (&ARCHIVE_LINK_NAME_LEAD,133,"db4f359289f869fb526cef48efbd431005fd97d968ce6a8071fc9c43fe410eac"),

            (&ARCHIVE_LINK_QUEUE_LEAD,137,"0f9c600ec7659a45d83068e55d5fbb91cfedcebf8cae31267c4b0d8315906846"),

            (&ARCHIVE_LINK_RERUN_LEAD,522,"8fd30046ebc56d296e265ff2ad9ce1a18b9d17a36be2f13874f1d4ddecbec4b7"),

            (&ARCHIVE_LINK_SET_BESIDE,179,"d3477aad234f337fae22c6fc7eeed164a0f8508eaf710d62b54a69d126bc44f0"),

            (&ARCHIVE_LINK_TRIED,95,"f81ca4eb115583670b3fc146d5eb67dce9671e9f7ed27186b80a7c1a3e91a4f0"),

            (&ARCHIVE_REV_DELETE_REV_VOLUMES,45,"4ae61f4e7937d224bd94ca39643feca937653645688a711c364c4224008961ea"),

            (&ARCHIVE_REV_INSERT_REV_VOLUME,92,"08974e7a845b33efa562d8760866ae8f4d03599a14b1d4f728fcb4490e50b815"),

            (&ARCHIVE_STORE_ADD_NAME_BYTES,129,"5d71cd6f37c698c2d5cbc7b9505f24705499c50409bdc2ecb463e99ff362c50e"),

            (&ARCHIVE_STORE_COUNT_OVER_QUOTA,121,"b03bcbf5ccc8cd7d146712d4e8ed90ea2907e35517ff4ec15e910d81061399fc"),

            (&ARCHIVE_STORE_DELETE_PAR2_FILES,44,"e4122b80993d6d2db88f0722fed65c58443981a87ffc178ad2f54a461a9e696d"),

            (&ARCHIVE_STORE_DELETE_SOURCE,69,"fbe2245996a55e35aa6a2702c43ea5e559e45c15f5b7c8449ce7cfb6c9895ead"),

            (&ARCHIVE_STORE_INSERT_FILE_FACTS,147,"f12cfe0efefb53c268fd0404d2411e535bc2b6ea3aa4c0f4a03c0ba7444b4cff"),

            (&ARCHIVE_STORE_INSERT_PAR2_FILE,183,"44f0a7d4939f680b26141a8a23f6eeafdc933a7aea91be013d55fdcb9bd6b322"),

            (&ARCHIVE_STORE_MARK_POINTED,99,"39df86e33086712a79db8c2fe83ca6c0f037e087a7f9672909b26c2af5b92946"),

            (&ARCHIVE_STORE_MARK_POINTED_PASSWORD,122,"9bae67b46b510993930a3a4afb1d45db54cb79ab2a14f56d4bcef726a368bbba"),

            (&ARCHIVE_STORE_RANK_POINTED,2141,"f67d86626c80ea497c0feb8a331a6d0e4f4c64dd73a6f9334f8b1dce2f59b908"),

            (&ARCHIVE_STORE_RERUN_POINTED_LIST,1504,"b7e5cae0e7a525510b39c9d435c21c40c7ae8531a649199f8b39507204d622ec"),

            (&ARCHIVE_STORE_RERUN_POINTED_PROBE,735,"f43a2b3beae0eeae14f25e27e383d791a2080a0d637153766bdce2646abab118"),

            (&ARCHIVE_STORE_SET_ENCRYPTION,104,"fbc749cdd71cb8f3c28cc24cff369bc7482381e727fcf6deda1f117005f310ea"),

            (&ARCHIVE_STORE_SET_KEYS,122,"cc4f9e002070ee518a2fe41b19d09f1eedc1f78f31f096fda8d23258ce6f89a2"),

            (&ARCHIVE_STORE_SET_NFO_PASSWORD,82,"4f669ee0f2ac3929b76dd3e0e8c03fd4cb4ae8a3110e17c05c773d5d105b05a1"),

            (&ARCHIVE_STORE_SET_PAR2_CREATOR,72,"14581d63412c35d6c827c31aa2408622f66e7823e33ff8acf21e2f7df0830212"),

            (&ARCHIVE_STORE_SET_SEGMENT_CRC,99,"f767f2d1266ea84592014867b7daa99ec886ee2f129e006846cff532f2e35ac4"),

            (&ARCHIVE_STORE_UPSERT_RELEASE,137,"ebe1d39bdeef9198f6ea52367ac2857dda97ba88e027cf0fc5bb2ce959ed4120"),

            (&DUPLICATES_CLEAR_CONTENT,67,"214032b82d006f270839ec25edeaeb4f2c13057cdb568dfadeaacb6302d7455d"),

            (&DUPLICATES_CLEAR_POSTED,109,"bad895f8ffbceab894d8370a8c9063c0c0b0c8e4ade715f6a1331ad7aea5dcd7"),

            (&DUPLICATES_SET_CONTENT,138,"787a82137165438279846e5a28667da26e4562048a7b562ab7b8bd8890cbdfe5"),

            (&DUPLICATES_SET_PAR2_SIZES,200,"c003a34437819e53681f14daeafebebf73513fad7062bc241ceb3052ee2ae6c4"),

            (&DUPLICATES_SET_POSTED,135,"ad20033376fb67d2df9a658b11c524671cea70c1de9d13a16a3bd77c27daa6cf"),

            (&JUNK_DROP_CATEGORY,50,"15e1cbcddac0ccfb5680a7c59121940a8369c9719be885de8d5f20f1d498e097"),

            (&KEYS_UPSERT,257,"8e8f51423f673e1388898604c960b7254593ae6998f1ab700f9cee97dcb223cf"),

            (&MEDIA_STEPS_FOUND,268,"d4e414635e386f72a1002fe14274cce9d6055ac14459f1e3c81d1aa65471e2d7"),

            (&MEDIA_STEPS_UPSERT,1867,"a519f46c2fd8d558399ace40fefe53e1c3d992be4453ac9b9c2e4ade8a36cb16"),

            (&PROBE_DROP_COVER,47,"6b14a1a7180e5c7d01294eafc987464179d55942abc85bf8e0a627585395da6d"),

            (&PROBE_SET_COVER,83,"4a96b1ba47aa81617fbb2045cc52fc6ee0355fd5a1e70e0971f9ecb17c23acfa"),

            (&PROBE_SET_PROBE,112,"d34784502fec00d90f9eb8d45f52213400b0ac1924a03bb748680da2efb17754"),

            (&PROBE_NFO_STORE_DROP,59,"4263ef437ed15518f76ab09253240a9056590bf37b6c2ce43c88dabcb3992a45"),

            (&SIDECAR_EVICT,445,"492e25d07b0c0425b4ad120d10b5599c8395b88baccf49baff311ac62565b8cd"),

            (&SIDECAR_SET,739,"1733553b0afab82d01e0a6f1c1bafee4ada9ebe3d845e0dcb5ca31f90ae9cdc1"),

            (&STORE_SET_VOUCHED_COMPLETE,1136,"e72687377cf3d51fbd44efdbb4b9de337f9447ee94e03748fa5fe425b051f3b8"),

        ];
        for (atom,length,expected) in atoms {
            assert_eq!(atom.len(),*length);
            assert_eq!(format!("{:x}",Sha256::digest(atom.as_bytes())),*expected);
        }

        assert_eq!(PROBE_NFO_STORE_MAX_CHARS,262144);

    }
}
