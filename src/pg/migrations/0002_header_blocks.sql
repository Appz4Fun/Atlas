-- Keep NXV payloads compressed and preserve their manifests for exact replay.
create table header_blocks (
    source       text collate "C" not null,
    group_name   text collate "C" not null,
    day          bigint not null,
    sequence     bigint not null check (sequence >= 0),
    header_count bigint not null check (header_count > 0),
    manifest     bytea not null check (octet_length(manifest) = 64),
    payload      bytea not null,
    primary key (source, group_name, day, sequence)
);

alter table header_blocks alter column payload set storage external;
