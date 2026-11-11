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
