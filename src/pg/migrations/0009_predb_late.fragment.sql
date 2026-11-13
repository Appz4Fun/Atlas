-- Fresh native Stage-owned derived PreDB nomination state. No old release,
-- queue or cursor import. Append only with the Stage9 owner's source ACK.
create sequence predb_name_revision;
create table predb_name_keys (
 key bigint not null, release_id bigint not null references releases(id) on delete cascade,
 primary key(key,release_id)
);
create table predb_name_pending (
 release_id bigint primary key references releases(id) on delete cascade
);
create table predb_late_scan (
 singleton boolean primary key check(singleton),
 title_id bigint not null references ext_predb_titles(id) on delete cascade,
 title_revision bigint not null,names_revision bigint not null,
 after_release bigint
);
create table predb_late_matches (
 release_id bigint primary key references releases(id) on delete cascade
);
create function atlas_predb_name_dirty() returns trigger language plpgsql as $$
begin
 perform nextval('predb_name_revision');
 if TG_OP<>'DELETE' then insert into predb_name_pending values(NEW.id) on conflict do nothing; end if;
 return null;
end $$;
create trigger native_predb_name_insert after insert or delete on releases
 for each row execute function atlas_predb_name_dirty();
create trigger native_predb_name_update after update of name,original_name,display_name,name_source on releases
 for each row execute function atlas_predb_name_dirty();
create function atlas_predb_inner_changed() returns trigger language plpgsql as $$
begin perform nextval('predb_name_revision'); return null; end $$;
create trigger native_predb_inner_changed after insert or update or delete on predb_inner_key
 for each row execute function atlas_predb_inner_changed();
create trigger native_predb_inner_truncated after truncate on predb_inner_key
 for each statement execute function atlas_predb_inner_changed();
create trigger native_predb_release_evidence after update of size on releases
 for each row execute function atlas_predb_inner_changed();
-- These facts participate in the real matcher evidence. A completed page
-- from an earlier evidence revision cannot count toward confirmed overflow.
create trigger native_predb_file_evidence after insert or update or delete on files
 for each row execute function atlas_predb_inner_changed();
create trigger native_predb_files_truncated after truncate on files
 for each statement execute function atlas_predb_inner_changed();
create trigger native_predb_archive_evidence after insert or update or delete on archive_inner_files
 for each row execute function atlas_predb_inner_changed();
create trigger native_predb_archive_truncated after truncate on archive_inner_files
 for each statement execute function atlas_predb_inner_changed();
revoke all on predb_name_keys,predb_name_pending,predb_late_scan,predb_late_matches,predb_name_revision from public;

do $$
declare relation text;
begin
 foreach relation in array array['predb_name_keys','predb_name_pending','predb_late_scan','predb_late_matches'] loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation);
 end loop;
end $$;
