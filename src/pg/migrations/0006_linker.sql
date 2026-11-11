-- Durable external ordering for the native linker. Group memberships are rows,
-- never arrays: a single poster/file/upload may contain arbitrarily many groups.
create table linker_epochs (
    id bigint generated always as identity primary key,
    logical_now bigint not null,
    phase text not null default 'stage' check (phase in ('stage','select','files','chain','plan','emit','ack','done')),
    active_serial bigint not null default 0,
    next_chain bigint not null default 0,
    emitted bigint not null default 0
);
create unique index linker_one_active_epoch on linker_epochs ((true)) where phase <> 'done';
create table linker_files (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    id bigint not null,
    poster bytea not null,
    run_no bigint not null default 0,
    first_rank bigint not null default 0,
    new_material boolean not null default true,
    again boolean not null default false,
    articles bigint not null check (articles > 0),
    first_posted bigint not null,
    last_posted bigint not null,
    planned boolean not null default false,
    primary key (epoch,id)
);
create index linker_file_order on linker_files (epoch,first_posted,(articles <> 1),last_posted,poster,id) where not planned;
create table linker_file_groups (
    epoch bigint not null,
    file_id bigint not null,
    group_id bigint not null,
    primary key (epoch,file_id,group_id),
    foreign key (epoch,file_id) references linker_files(epoch,id) on delete cascade
);
create table linker_chains (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    id bigint not null,
    seed bigint,
    first_posted bigint not null,
    last_posted bigint not null,
    index_only boolean not null,
    active_order bigint,
    file_count bigint not null default 0,
    primary key (epoch,id)
);
create index linker_chain_window on linker_chains (epoch,first_posted,last_posted);
create table linker_chain_groups (
    epoch bigint not null,
    chain_id bigint not null,
    -- 0: current groups; 1: singleton index groups; 2: original seed groups.
    kind smallint not null check (kind between 0 and 2),
    group_id bigint not null,
    primary key (epoch,chain_id,kind,group_id),
    foreign key (epoch,chain_id) references linker_chains(epoch,id) on delete cascade
);
create table linker_chain_files (
    epoch bigint not null,
    chain_id bigint not null,
    ordinal bigint not null,
    file_id bigint not null,
    primary key (epoch,chain_id,ordinal),
    unique (epoch,file_id),
    foreign key (epoch,chain_id) references linker_chains(epoch,id) on delete cascade,
    foreign key (epoch,file_id) references linker_files(epoch,id) on delete cascade
);

-- Scanned source progress is independent of release publication. Rows outside
-- the ready window survive here, so acknowledging a fully staged mixed block
-- cannot lose a future hour or a delayed chain.
create table linker_source_blocks (
    id bigint generated always as identity primary key,
    group_name text not null,
    day bigint not null,
    generation bytea not null,
    seq bigint not null,
    digest bytea not null,
    rows bigint not null,
    scanned bigint not null default 0,
    complete boolean not null default false,
    unique(group_name,day,generation,seq,digest),
    check(scanned between 0 and rows)
);
create table linker_epoch_sources (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    source bigint not null references linker_source_blocks(id),
    old_block boolean not null,
    primary key(epoch,source)
);
create table linker_staged_rows (
    source bigint not null references linker_source_blocks(id) on delete cascade,
    ordinal bigint not null,
    message_key bytea not null,
    legacy_hash bigint not null,
    message_id bytea not null,
    poster bytea not null,
    posted bigint not null,
    group_id bigint not null,
    article_number bigint not null,
    bytes bigint not null,
    lines bigint not null,
    primary key(source,ordinal)
);
create index linker_staged_message on linker_staged_rows(message_key);
create index linker_staged_posted on linker_staged_rows(posted);

-- Legacy logs only contain 64-bit hashes. Keep them separately: they cannot be
-- upgraded into exact IDs. A legacy collision conservatively suppresses both
-- IDs until expiry; newly linked IDs use their full canonical identity.
create table linker_legacy_ids (
    origin text not null,
    hash bigint not null,
    added bigint not null,
    primary key(origin,hash)
);
create index linker_legacy_ids_expiry on linker_legacy_ids(added);
create table linker_id_imports (
    name text primary key,
    source_identity bytea not null,
    offset_bytes bigint not null default 0,
    id_time bigint,
    complete boolean not null default false
);
create table linker_linked_ids (
    message_key bytea primary key,
    message_id bytea not null,
    added bigint not null
);
create index linker_linked_ids_expiry on linker_linked_ids(added);

-- Emission references immutable staged source rows; no duplicate Article tree
-- is serialized. File/part decisions are sealed before phase emit is entered.
create table linker_emission_files (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    id bigint not null,
    release_name text not null,
    group_name text not null,
    release_poster bytea not null,
    release_date text not null,
    poster bytea not null,
    file_number bigint not null,
    file_total bigint not null,
    part_total bigint not null,
    primary key(epoch,id)
);
create table linker_emission_rows (
    epoch bigint not null,
    ordinal bigint not null,
    file_id bigint not null,
    source bigint not null,
    source_ordinal bigint not null,
    part bigint not null,
    primary key(epoch,ordinal),
    unique(epoch,source,source_ordinal),
    foreign key(epoch,file_id) references linker_emission_files(epoch,id),
    foreign key(source,source_ordinal) references linker_staged_rows(source,ordinal)
);

-- Selection is frozen before inference: late placement/context filtering has
-- already classified these as fresh rows. Preserve source rank for stable ties.
create table linker_selected_rows (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    source bigint not null,
    source_ordinal bigint not null,
    source_rank bigint not null,
    again boolean not null,
    is_new boolean not null,
    primary key(epoch,source,source_ordinal),
    unique(epoch,source_rank),
    foreign key(source,source_ordinal) references linker_staged_rows(source,ordinal)
);
create table linker_file_rows (
    epoch bigint not null,
    file_id bigint not null,
    source bigint not null,
    source_ordinal bigint not null,
    source_rank bigint not null,
    primary key(epoch,source,source_ordinal),
    foreign key(epoch,file_id) references linker_files(epoch,id) on delete cascade,
    foreign key(source,source_ordinal) references linker_staged_rows(source,ordinal)
);
create index linker_file_rows_order on linker_file_rows(epoch,file_id,source_rank);

alter table linker_epochs add column lo bigint;
alter table linker_epochs add column hi bigint;
alter table linker_epochs add column waiting_from bigint;
alter table linker_source_blocks add column was_read boolean not null default false;
create table linker_linked_hours (hour bigint primary key);
create table linker_day_seen (day bigint primary key,first_seen bigint not null);
create table linker_epoch_hours (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    hour bigint not null,
    primary key(epoch,hour)
);
create table linker_epoch_inventory (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    group_name text collate "C" not null,
    day bigint not null,
    generation bytea not null,
    blocks bigint not null,
    scanned_blocks bigint not null default 0,
    primary key(epoch,group_name,day),
    check(scanned_blocks between 0 and blocks)
);
-- A sealed inventory is a durable segment high-watermark. It is separate from
-- the scanned block receipts and from the eventual publication acknowledgements.
alter table linker_epochs add column inventory_sealed boolean not null default false;
create table linker_inventory_blocks (
    epoch bigint not null,
    group_name text not null,
    day bigint not null,
    seq bigint not null,
    source bigint not null references linker_source_blocks(id),
    primary key(epoch,group_name,day,seq),
    foreign key(epoch,group_name,day) references linker_epoch_inventory(epoch,group_name,day) on delete cascade
);
-- Pending inference rows outlive their NXV segment. Every epoch snapshots this
-- inbox as well as current raw segments, before complete staged segments expire.
alter table linker_staged_rows add column published boolean not null default false;
create index linker_pending_source on linker_staged_rows(source) where not published;
create table linker_epoch_pending_sources (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    source bigint not null references linker_source_blocks(id),
    primary key(epoch,source)
);
create table linker_places (
    poster bytea primary key,
    release_name text not null,
    group_name text not null,
    file_number bigint not null,
    file_total bigint not null,
    parts bigint not null,
    oldest bigint not null,
    added bigint not null
);
create table linker_uploads (
    release_name text primary key,
    group_name text not null,
    first_posted bigint not null,
    last_posted bigint not null,
    files bigint not null,
    generation bigint not null,
    probed bigint not null,
    added bigint not null
);
create table linker_upload_groups (
    release_name text not null references linker_uploads(release_name) on delete cascade,
    group_id bigint not null,
    primary key(release_name,group_id)
);
create table linker_place_firsts (
    poster bytea not null references linker_places(poster) on delete cascade,
    group_id bigint not null,
    article_number bigint not null,
    primary key(poster,group_id)
);
create table linker_place_leads (
    poster bytea not null references linker_places(poster) on delete cascade,
    ordinal bigint not null,
    part bigint not null,
    primary key(poster,ordinal)
);
create table linker_candidates (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    source bigint not null,
    source_ordinal bigint not null,
    source_rank bigint not null,
    poster_rank bigint not null,
    is_new boolean not null,
    primary key(epoch,source,source_ordinal),
    unique(epoch,source_rank),
    foreign key(source,source_ordinal) references linker_staged_rows(source,ordinal)
);
alter table linker_selected_rows add column poster_rank bigint not null default 0;
create table linker_late_rows (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    source bigint not null,
    source_ordinal bigint not null,
    source_rank bigint not null,
    poster_rank bigint not null,
    primary key(epoch,source,source_ordinal),
    foreign key(source,source_ordinal) references linker_staged_rows(source,ordinal)
);
