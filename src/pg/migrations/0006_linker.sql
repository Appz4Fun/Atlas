-- Durable external ordering for the native linker. Group memberships are rows,
-- never arrays: a single poster/file/upload may contain arbitrarily many groups.
create table linker_epochs (
    id bigint generated always as identity primary key,
    logical_now bigint not null,
    phase text not null default 'stage' check (phase in ('stage','files','chain','emit','ack','done')),
    active_serial bigint not null default 0,
    next_chain bigint not null default 0
);
create unique index linker_one_active_epoch on linker_epochs ((true)) where phase <> 'done';
create table linker_files (
    epoch bigint not null references linker_epochs(id) on delete cascade,
    id bigint not null,
    poster text collate "C" not null,
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
