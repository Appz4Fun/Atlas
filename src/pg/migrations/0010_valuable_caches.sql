-- Costly, natural-key cache facts survive a fresh release/header catalog.
-- Preserve original values/NULLs/opaque versions; importing bytes never makes
-- them eligible for reuse without the existing content/provenance checks.
create table ext_raw_probes (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 crc32 bigint,agent text,info text not null,moviehash text,parts bigint,complete bigint,
 config_stamp bigint,probed_at bigint not null,
 primary key(natural_name_key,natural_group_key)
);
alter table ext_raw_probes alter column info set storage external;
create index ext_raw_probes_content on ext_raw_probes(sha256(content_id)) where content_id is not null;
create table ext_raw_covers (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 mime text not null,sha256 text not null,primary key(natural_name_key,natural_group_key)
);
create table ext_raw_archives (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 password bigint not null,encryption bigint,names_hidden bigint,password_check bytea,
 parts bigint,complete bigint,listing_complete bigint,listing_damaged bigint,content_fp bytea,
 packer text,twin_key bigint,owner_key bigint,zone_key bigint,key_salt bigint,facts bigint,
 primary key(natural_name_key,natural_group_key)
);
create table ext_raw_archive_inner_files (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 release_name text collate "C" not null,group_name text collate "C" not null,seq bigint not null,
 content_id bytea,name text not null,size bigint,source bigint,crc32 bigint,mtime bigint,host_os bigint,
 primary key(natural_name_key,natural_group_key,seq)
);
create table ext_raw_par2 (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 covers_all bigint not null default 0,posted bytea,par2_block bigint,par2_blocks bigint,
 parts bigint,complete bigint,content_fp bytea,primary key(natural_name_key,natural_group_key)
);
create table ext_raw_par2_files (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 natural_file_key bytea not null default ''::bytea check(octet_length(natural_file_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 file_id bytea not null,set_id bytea not null,md5 bytea not null,md5_16k bytea not null,
 size bigint not null,file_name text not null,crc32 bigint,bound bigint not null default 1,
 sealed bigint not null default 0,complete bigint not null default 0,creator text,
 primary key(natural_name_key,natural_group_key,natural_file_key)
);
create table ext_raw_rev_volumes (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 volume bigint not null,size bigint not null,crc32 bigint not null,
 primary key(natural_name_key,natural_group_key,volume)
);
create table ext_raw_nfo_passwords (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 password text not null,primary key(natural_name_key,natural_group_key)
);
create table ext_raw_stills (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,position bigint not null,
 content_id bytea,at_ms bigint,sha256 text not null,width bigint,height bigint,phash bigint,
 primary key(natural_name_key,natural_group_key,position)
);
create table ext_raw_audio_prints (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,position bigint not null,
 content_id bytea,at_ms bigint not null,items bytea not null,
 primary key(natural_name_key,natural_group_key,position)
);
alter table ext_raw_audio_prints alter column items set storage external;

-- Non-pruning extraction may find older/different useful facts at the same
-- natural key. Keep those original typed rows as preservation records rather
-- than overwriting them or treating them as active cache hits. No release FK.
-- row_format=1 is UTF-8 PostgreSQL canonical row JSON; schema_descriptor is
-- ordered (column-name, PostgreSQL-type) pairs. bytea remains its exact hex
-- bytes, with bigint/NULL/text distinguished. This is inert preservation;
-- source_digest identifies an input namespace, not source-completeness proof.
create table ext_preserved_cache_rows (
 family text collate "C" not null,source_digest bytea not null check(octet_length(source_digest)=32),
 row_digest bytea not null check(octet_length(row_digest)=32),
 name text collate "C",group_name text collate "C",content_id bytea,natural_name_key bytea,natural_group_key bytea,
 schema_descriptor text not null,row_format bigint not null check(row_format=1),row_bytes bytea not null,
 primary key(family,source_digest,row_digest)
);
alter table ext_preserved_cache_rows alter column row_bytes set storage external;
create index ext_preserved_cache_natural on ext_preserved_cache_rows(family,natural_name_key,natural_group_key);

-- Original private cache-derivation keys, separate from fresh native catalog
-- allocators/cursors. Runtime must explicitly select a compatible namespace;
-- restoring this table never changes a new install's identity automatically.
create table ext_cache_identity_keys (
 source_digest bytea not null check(octet_length(source_digest)=32),
 key text collate "C" not null check(key in ('key_salt','id_key0','id_key1')),
 value bigint not null,primary key(source_digest,key)
);
create table ext_segment_crc_cache (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 natural_message_key bytea not null default ''::bytea check(octet_length(natural_message_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 message_id text collate "C" not null,decoded bigint not null,crc32 bigint not null,
 primary key(natural_name_key,natural_group_key,natural_message_key,decoded,crc32)
);
create table ext_cached_images (
 natural_name_key bytea not null default ''::bytea check(octet_length(natural_name_key)=32),
 natural_group_key bytea not null default ''::bytea check(octet_length(natural_group_key)=32),
 name text collate "C" not null,group_name text collate "C" not null,content_id bytea,
 position bigint not null,file text not null,sha256 text collate "C" not null,format text not null,
 width bigint not null,height bigint not null,bytes bigint not null,meta text,exif_id text,document_id text,
 primary key(natural_name_key,natural_group_key,position,sha256)
);
-- The file copier supplies actual independently checked digests; a database
-- import/receipt alone never proves a filesystem asset was copied correctly.
create table ext_asset_files (
 kind text collate "C" not null check(kind in ('posters','covers','stills','images')),
 name text collate "C" not null,byte_length bigint not null check(byte_length>=0),
 digest bytea not null check(octet_length(digest)=32),source_digest bytea not null check(octet_length(source_digest)=32),
 primary key(kind,name)
);

-- Derived fixed-size index keys are never caller authority. Full original
-- values remain stored and consumers compare them after the indexed lookup.
-- The trigger replaces supplied/default key bytes before constraints are tested.
create function atlas_cache_natural_keys() returns trigger language plpgsql as $$
begin
 if TG_TABLE_NAME='ext_raw_archive_inner_files' then
  NEW.natural_name_key:=sha256(convert_to(NEW.release_name,'UTF8'));
 else NEW.natural_name_key:=sha256(convert_to(NEW.name,'UTF8'));
 end if;
 NEW.natural_group_key:=sha256(convert_to(NEW.group_name,'UTF8'));
 if TG_TABLE_NAME='ext_raw_par2_files' then NEW.natural_file_key:=sha256(NEW.file_id); end if;
 if TG_TABLE_NAME='ext_segment_crc_cache' then NEW.natural_message_key:=sha256(convert_to(NEW.message_id,'UTF8')); end if;
 return NEW;
end $$;
do $$ declare relation text; begin
 foreach relation in array array['ext_raw_probes','ext_raw_covers','ext_raw_archives','ext_raw_archive_inner_files',
  'ext_raw_par2','ext_raw_par2_files','ext_raw_rev_volumes','ext_raw_nfo_passwords','ext_raw_stills','ext_raw_audio_prints',
  'ext_preserved_cache_rows','ext_segment_crc_cache','ext_cached_images'] loop
  execute format('create trigger cache_natural_keys before insert or update on %I for each row execute function atlas_cache_natural_keys()',relation);
 end loop;
end $$;

-- Same existing RC/schema-fence guard as ext7, including direct DML/TRUNCATE.
do $$ declare relation text; begin
 foreach relation in array array['ext_raw_probes','ext_raw_covers','ext_raw_archives','ext_raw_archive_inner_files',
  'ext_raw_par2','ext_raw_par2_files','ext_raw_rev_volumes','ext_raw_nfo_passwords','ext_raw_stills','ext_raw_audio_prints',
  'ext_preserved_cache_rows','ext_cache_identity_keys','ext_segment_crc_cache','ext_cached_images','ext_asset_files'] loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation);
  execute format('revoke all on %I from public',relation);
 end loop;
end $$;
