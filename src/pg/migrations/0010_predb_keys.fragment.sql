-- Append to the fresh valuable-cache migration. This is a derived matcher index;
-- raw PreDB titles/feed positions remain the preservation authority.
create sequence ext_predb_key_revision;
create table ext_predb_title_keys (
    key bigint not null,
    title_id bigint not null references ext_predb_titles(id) on delete cascade,
    primary key(key,title_id)
);
create table ext_predb_key_meta (
    singleton bigint primary key check(singleton=1),
    version bigint not null check(version=1),
    revision bigint not null check(revision>0),
    top_id bigint,
    title_rows bigint not null check(title_rows>=0),
    key_rows bigint not null check(key_rows>=0)
);
create table ext_predb_key_build (
    singleton bigint primary key check(singleton=1),
    after_id bigint,
    title_rows bigint not null check(title_rows>=0),
    key_rows bigint not null check(key_rows>=0)
);
create function ext_predb_invalidate_source() returns trigger language plpgsql as $$
begin delete from ext_predb_key_meta; delete from ext_predb_key_build; return null; end $$;
create function ext_predb_invalidate_keys() returns trigger language plpgsql as $$
begin delete from ext_predb_key_meta; return null; end $$;
create trigger ext_predb_titles_keys_dirty after insert or update or delete or truncate on ext_predb_titles
    for each statement execute function ext_predb_invalidate_source();
create trigger ext_predb_derived_keys_dirty after insert or update or delete or truncate on ext_predb_title_keys
    for each statement execute function ext_predb_invalidate_keys();
revoke all on ext_predb_title_keys,ext_predb_key_meta,ext_predb_key_build,ext_predb_key_revision from public;

do $$
declare relation text;
begin
 foreach relation in array array['ext_predb_title_keys','ext_predb_key_meta','ext_predb_key_build'] loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation);
 end loop;
end $$;
