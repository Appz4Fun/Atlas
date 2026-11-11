-- Hidden, source-qualified historical identity. No state in this migration
-- enables native writes or a public reader.
create table history_companion_receipts (
    owner_code text primary key check(owner_code in ('main_import','ext')),
    source_digest bytea not null check(octet_length(source_digest)=32),
    receipt_digest bytea not null check(octet_length(receipt_digest)=32),
    manifest bytea not null check(octet_length(manifest)<=65536),
    dirty boolean not null default false,
    derived_allocator bigint check(derived_allocator>0)
);
revoke all on history_companion_receipts from public;
create table history_imports (
    import_id bytea primary key check(octet_length(import_id)=32),
    source_fingerprint bytea not null check(octet_length(source_fingerprint)=32),
    contract_version integer not null check(contract_version=1),
    state text not null check(state in ('loading','identity_ready','failed')),
    expected_database text not null, expected_schema text not null,
    expected_role text not null, expected_owner text not null,
    reserved_next_seq bigint not null check(reserved_next_seq>0),
    checkpoint_sequence bigint not null default 0 check(checkpoint_sequence>=0),
    checkpoint bytea not null,
    manifest_object_id bigint,
    identity_digest bytea check(octet_length(identity_digest)=32),
    failure_code text
);
create unique index history_single_import on history_imports((true));
create table history_sources (
    import_id bytea not null references history_imports,
    source_id bigint not null check(source_id>=0),
    kind text not null check(kind in ('manifest','main','current','frozen')),
    shard_number integer check(shard_number>=0),
    paired_current_source bigint,
    file_manifest_digest bytea not null check(octet_length(file_manifest_digest)=32),
    schema_digest bytea not null check(octet_length(schema_digest)=32),
    facts_object_id bigint,
    primary key(import_id,source_id),
    foreign key(import_id,paired_current_source) references history_sources deferrable initially deferred
);
create table history_objects (
    import_id bytea not null,
    object_id bigint not null check(object_id>0),
    source_id bigint not null,
    scope_release_id bigint not null,
    object_kind text not null check(object_kind in ('manifest','facts','metadata','frozen_header','frozen_payload','frozen_metadata','cell')),
    source_bookmark bytea not null check(octet_length(source_bookmark)<=512),
    locator_digest bytea not null check(octet_length(locator_digest)=32),
    byte_length bigint not null check(byte_length>=0),
    digest bytea check(octet_length(digest)=32),
    validated boolean not null default false,
    logical_rows bigint check(logical_rows>=0),logical_values bigint check(logical_values>=0),
    primary key(import_id,object_id),
    unique(import_id,source_id,object_kind,locator_digest),
    foreign key(import_id,source_id) references history_sources deferrable initially deferred,
    check(not validated or digest is not null)
);
create index history_objects_provisional on history_objects(import_id,object_id) where not validated;
create table history_object_chunks (
    import_id bytea not null, object_id bigint not null,
    ordinal bigint not null check(ordinal>=0),byte_offset bigint not null check(byte_offset>=0),
    bytes bytea not null check(octet_length(bytes) between 1 and 65536),
    digest bytea not null check(octet_length(digest)=32),
    primary key(import_id,object_id,ordinal),unique(import_id,object_id,byte_offset),
    foreign key(import_id,object_id) references history_objects deferrable initially deferred
);
alter table history_sources add foreign key(import_id,facts_object_id) references history_objects deferrable initially deferred;
alter table history_imports add foreign key(import_id,manifest_object_id) references history_objects deferrable initially deferred;
create table history_release_variants (
    import_id bytea not null, source_id bigint not null, public_id bigint not null,
    metadata_object_id bigint not null, frozen_header_object_id bigint, frozen_payload_object_id bigint,
    primary key(import_id,source_id,public_id),
    foreign key(import_id,source_id) references history_sources,
    foreign key(import_id,metadata_object_id) references history_objects,
    foreign key(import_id,frozen_header_object_id) references history_objects,
    foreign key(import_id,frozen_payload_object_id) references history_objects
);
create index history_variants_public on history_release_variants(import_id,public_id,source_id);
create table history_catalog (
    import_id bytea not null,public_id bigint not null,selected_source_id bigint not null,
    primary key(import_id,public_id),
    foreign key(import_id,selected_source_id,public_id) references history_release_variants(import_id,source_id,public_id)
);
create table history_frozen_resolutions (
    import_id bytea not null,source_id bigint not null,public_id bigint not null,
    header_object_id bigint not null,metadata_object_id bigint not null,
    differing_columns bytea not null check(octet_length(differing_columns)<=2048),
    oracle_code text not null check(oracle_code='embedded_release_thaw_v1'),
    primary key(import_id,source_id,public_id),
    foreign key(import_id,source_id,public_id) references history_release_variants,
    foreign key(import_id,header_object_id) references history_objects,
    foreign key(import_id,metadata_object_id) references history_objects
);
create table history_collection_aliases (
    import_id bytea not null,source_id bigint not null,original_signed_hash bigint not null,target_public_id bigint not null,
    primary key(import_id,source_id,original_signed_hash),
    foreign key(import_id,source_id) references history_sources
    -- No target FK: stale/dangling aliases are preserved source facts.
);
create index history_alias_targets on history_collection_aliases(import_id,target_public_id,source_id);
create table history_checkpoints (
    import_id bytea not null references history_imports,sequence bigint not null check(sequence>0),
    bookmark bytea not null,source_rows bigint not null check(source_rows>=0),
    source_bytes bigint not null check(source_bytes>=0),decoded_bytes bigint not null check(decoded_bytes>=0),
    callback_events bigint not null check(callback_events>=0),mutations bigint not null check(mutations>0),
    copy_bytes bigint not null check(copy_bytes>=0),digest bytea not null check(octet_length(digest)=32),
    primary key(import_id,sequence)
);
create table history_receipts (
    import_id bytea not null,source_id bigint not null,relation_code integer not null,
    relation_name bytea not null check(octet_length(relation_name)<=256),
    state text not null check(state in ('pending','complete')),
    logical_rows bigint check(logical_rows>=0),digest bytea check(octet_length(digest)=32),
    check(state<>'complete' or (logical_rows is not null and digest is not null)),
    owner_code text not null,
    primary key(import_id,source_id,relation_code),
    foreign key(import_id,source_id) references history_sources
);

-- Application roles receive no history privileges through PUBLIC. The schema
-- owner is the conversion authority; runtime access uses guarded Rust handles.
revoke all on history_imports,history_sources,history_objects,history_object_chunks,
    history_release_variants,history_catalog,history_collection_aliases,
    history_checkpoints,history_receipts,history_frozen_resolutions from public;

-- Statement triggers supplement explicit Rust transaction entry guards. They
-- also cover direct Sql cursor/metadata entrypoints. Volatile function queries
-- recheck the gate after the ownership fence; import registration takes the
-- identical fence before inserting its persistent gate.
create function atlas_native_write_guard(p_schema text) returns void language plpgsql volatile
security definer set search_path=pg_catalog as $$
declare blocked boolean;
begin
    execute format('select exists(select 1 from %I.history_imports)',p_schema) into blocked;
    if blocked then raise exception 'historical import blocks native mutations' using errcode='55000'; end if;
    perform pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer'),pg_catalog.hashtext(p_schema));
    execute format('select exists(select 1 from %I.history_imports)',p_schema) into blocked;
    if blocked then raise exception 'historical import blocks native mutations' using errcode='55000'; end if;
end $$;
create function atlas_native_write_trigger() returns trigger language plpgsql volatile
security definer set search_path=pg_catalog as $$
begin
    execute format('select %I.atlas_native_write_guard($1)',TG_TABLE_SCHEMA) using TG_TABLE_SCHEMA;
    if TG_TABLE_NAME=any(array['groups','meta','backfill_chunks','backfill_deepest','backfill_media','backfill_speculative','backfill_sweep_gen','backfill_sweeps','backfill_wanted','spot_blacklist','spot_comments','spot_disposals','spot_reports','spots_seen','inferred_asked','inferred_ids','repair_tried','repair_older']) then
        execute format('update %I.history_companion_receipts set dirty=true where owner_code=''main_import''',TG_TABLE_SCHEMA);
    elsif left(TG_TABLE_NAME,4)='ext_' then
        execute format('update %I.history_companion_receipts set dirty=true where owner_code=''ext''',TG_TABLE_SCHEMA);
    end if;
    return null;
end $$;
do $$
declare relation record;
begin
    for relation in select c.relname from pg_class c join pg_namespace n on n.oid=c.relnamespace
        where n.nspname=current_schema() and c.relkind='r'
        and c.relname<>'schema_migrations' and c.relname not like 'history\_%' escape '\'
    loop
        execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation.relname);
    end loop;
end $$;
