-- Full fresh-native workers, on the exact published Stage9/cache10 base.
-- Valuable caches are retained. Old release/header/queue progress is never
-- silently adopted or backfilled into the new worker input contract.
do $$ declare relation text; occupied boolean; begin
 if not pg_try_advisory_xact_lock(hashtext('atlas.indexer'),hashtext(current_schema())) then
  raise exception 'stop the native owner before full worker migration' using errcode='55000';
 end if;
 foreach relation in array array['releases','files','segments','header_blocks','pp_queue','pp_state','pp_walks',
  'native_operations','word_queue','release_writer_junk','junk_releases','junk_collections',
  'groups','backfill_chunks','backfill_deepest','backfill_media','backfill_speculative','backfill_sweep_gen',
  'backfill_sweeps','backfill_wanted','spots','spot_comments','spot_disposals','spot_reports','spots_seen',
  'inferred_asked','inferred_ids','repair_tried','repair_older','history_imports'] loop
  execute format('select exists(select 1 from %I)',relation) into occupied;
  if occupied then raise exception 'full native workers require a fresh release and operational catalog; retain valuable caches separately' using errcode='55000'; end if;
 end loop;
end $$;

-- Indexed own-file proof and bounded retry expiration; no old-catalog backfill.
create index native_segments_file_message on segments(file_id,message_key);
create index native_inferred_ids_next on inferred_ids(next_at,message_id);
create index native_inferred_asked_next on inferred_asked(next_at,release_id);

-- Spotnet originals are bytes: display-safe text never replaces XML/body evidence.
alter table spots add column native_original_xml bytea;
alter table spot_comments add column native_original_body bytea;
create index spots_message_id on spots(message_id);
create index spots_nzb_key on spots(nzb_key);
create index spots_xml_missing on spots(release_id) where xml is null;

-- The opening fresh-catalog guard witnessed empty releases and chunk tables.
-- Maintain only small per-group totals; observation never scans the catalog.
create table native_metric_catalog_groups (
 group_name text collate "C" primary key,
 total bigint not null check(total>0),bytes bigint not null check(bytes>=0),
 complete bigint not null check(complete between 0 and total),
 nzb_ready bigint not null check(nzb_ready between 0 and total),
 named bigint not null check(named between 0 and total),
 still_obfuscated bigint not null check(still_obfuscated between 0 and total),
 predb_matched bigint not null check(predb_matched between 0 and total)
);
create function atlas_native_catalog_metric_delta(g text,n bigint,b bigint,c boolean,p bigint,d text,o boolean,m boolean)
returns void language plpgsql as $$
declare remaining bigint;
begin
 if n=1 then
  insert into native_metric_catalog_groups as old values(g,1,greatest(b,0),c::int,(p>0)::int,(d is not null)::int,(o and d is null)::int,m::int)
  on conflict(group_name) do update set total=old.total+1,bytes=old.bytes+excluded.bytes,
   complete=old.complete+excluded.complete,nzb_ready=old.nzb_ready+excluded.nzb_ready,
   named=old.named+excluded.named,still_obfuscated=old.still_obfuscated+excluded.still_obfuscated,predb_matched=old.predb_matched+excluded.predb_matched;
 elsif n=-1 then
  -- Remove the last group row directly: the positive-total constraint also
  -- refuses stale or invented zero summary rows.
  delete from native_metric_catalog_groups where group_name=g and total=1;
  if found then return; end if;
  update native_metric_catalog_groups set total=total-1,bytes=bytes-greatest(b,0),
   complete=complete-c::int,nzb_ready=nzb_ready-(p>0)::int,named=named-(d is not null)::int,
   still_obfuscated=still_obfuscated-(o and d is null)::int,predb_matched=predb_matched-m::int
   where group_name=g returning total into remaining;
  if not found then raise exception 'native catalog summary is missing' using errcode='55000'; end if;
 else raise exception 'invalid native catalog summary delta' using errcode='55000'; end if;
end $$;
create function atlas_native_catalog_metrics() returns trigger language plpgsql as $$
begin
 if tg_op='TRUNCATE' then delete from native_metric_catalog_groups;return null;end if;
 if tg_op in('UPDATE','DELETE') then
  perform atlas_native_catalog_metric_delta(old.group_name,-1,old.size,old.complete,old.parts,old.display_name,old.is_obfuscated,
   exists(select 1 from predb_match where release_id=old.id));
 end if;
 if tg_op in('INSERT','UPDATE') then
  perform atlas_native_catalog_metric_delta(new.group_name,1,new.size,new.complete,new.parts,new.display_name,new.is_obfuscated,
   exists(select 1 from predb_match where release_id=new.id));
 end if;
 return coalesce(new,old);
end $$;
create trigger native_catalog_metric_insert after insert on releases for each row execute function atlas_native_catalog_metrics();
create trigger native_catalog_metric_update after update of group_name,size,complete,parts,display_name,is_obfuscated on releases for each row execute function atlas_native_catalog_metrics();
-- BEFORE DELETE observes the actual match before its FK cascade removes it.
create trigger native_catalog_metric_delete before delete on releases for each row execute function atlas_native_catalog_metrics();
create trigger native_catalog_metric_truncate after truncate on releases for each statement execute function atlas_native_catalog_metrics();
create function atlas_native_catalog_match_metrics() returns trigger language plpgsql as $$
begin
 if tg_op='TRUNCATE' then update native_metric_catalog_groups set predb_matched=0;return null;end if;
 if tg_op in('UPDATE','DELETE') then
  update native_metric_catalog_groups s set predb_matched=s.predb_matched-1
   from releases r where r.id=old.release_id and s.group_name=r.group_name;
 end if;
 if tg_op in('INSERT','UPDATE') then
  update native_metric_catalog_groups s set predb_matched=s.predb_matched+1
   from releases r where r.id=new.release_id and s.group_name=r.group_name;
 end if;
 return coalesce(new,old);
end $$;
create trigger native_catalog_match_metric_row after insert or update or delete on predb_match for each row execute function atlas_native_catalog_match_metrics();
create trigger native_catalog_match_metric_truncate after truncate on predb_match for each statement execute function atlas_native_catalog_match_metrics();

create table native_metric_chunk_groups (
 grp text collate "C" primary key,total bigint not null check(total>0),done bigint not null check(done between 0 and total),
 oldest_day bigint not null,newest_day bigint not null,oldest_done_day bigint
);
-- A statement may remove/move every row before queued AFTER ROW events run.
-- Keep exact row deltas separate from final boundary reads. Only distinct
-- touched keys are retained; no transition-table payload or global recount.
create table native_metric_chunk_touched (
 txid bigint not null,grp text collate "C" not null,primary key(txid,grp)
);
create function atlas_native_chunk_metric_delta(g text,n bigint,d bigint,s bigint) returns void language plpgsql as $$
begin
 insert into native_metric_chunk_touched(txid,grp) values(txid_current(),g) on conflict do nothing;
 if n=1 then
  insert into native_metric_chunk_groups as old values(g,1,(s=2)::int,d,d,case when s=2 then d end)
  on conflict(grp) do update set total=old.total+1,done=old.done+excluded.done;
 elsif n=-1 then
  delete from native_metric_chunk_groups where grp=g and total=1;
  if found then return;end if;
  update native_metric_chunk_groups set total=total-1,done=done-(s=2)::int where grp=g;
  if not found then raise exception 'native chunk summary is missing' using errcode='55000';end if;
 else raise exception 'invalid native chunk summary delta' using errcode='55000';end if;
end $$;
create function atlas_native_chunk_metrics() returns trigger language plpgsql as $$
begin
 if tg_op='TRUNCATE' then delete from native_metric_chunk_groups;delete from native_metric_chunk_touched where txid=txid_current();return null;end if;
 if tg_op in('UPDATE','DELETE') then perform atlas_native_chunk_metric_delta(old.grp,-1,old.day,old.state);end if;
 if tg_op in('INSERT','UPDATE') then perform atlas_native_chunk_metric_delta(new.grp,1,new.day,new.state);end if;
 return coalesce(new,old);
end $$;
create function atlas_native_chunk_metrics_finish() returns trigger language plpgsql as $$
declare group_key text;first_day bigint;last_day bigint;done_day bigint;affected integer:=0;
begin
 for group_key in select grp from native_metric_chunk_touched where txid=txid_current() order by grp loop
  affected:=affected+1;
  if affected>10000 then raise exception 'native chunk boundary inventory exceeds admission' using errcode='55000';end if;
  delete from native_metric_chunk_touched where txid=txid_current() and grp=group_key;
  -- Existing (grp,day)/(grp,state,day) indexes supply bounded boundary seeks.
  -- Do not install NULL or invented zero boundaries while row deltas drain.
  select day into first_day from backfill_chunks where grp=group_key order by day limit 1;
  if not found then
   if exists(select 1 from native_metric_chunk_groups where grp=group_key) then
    raise exception 'native chunk summary retained an empty group' using errcode='55000';
   end if;
  else
   select day into last_day from backfill_chunks where grp=group_key order by day desc limit 1;
   select day into done_day from backfill_chunks where grp=group_key and state=2 order by day limit 1;
   update native_metric_chunk_groups set oldest_day=first_day,newest_day=last_day,oldest_done_day=done_day
    where grp=group_key and ((done=0)=(done_day is null));
   if not found then raise exception 'native chunk summary or done boundary is inconsistent' using errcode='55000';end if;
  end if;
 end loop;
 return null;
end $$;
create trigger native_chunk_metric_row after insert or update or delete on backfill_chunks for each row execute function atlas_native_chunk_metrics();
create trigger native_chunk_metric_finish after insert or update or delete on backfill_chunks for each statement execute function atlas_native_chunk_metrics_finish();
create trigger native_chunk_metric_truncate after truncate on backfill_chunks for each statement execute function atlas_native_chunk_metrics();
do $$ declare relation text;begin
 foreach relation in array array['native_metric_catalog_groups','native_metric_chunk_groups','native_metric_chunk_touched'] loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation);
 end loop;
end $$;

-- Display strings can sanitize NUL for PostgreSQL text. Keep original bytes for
-- exact natural-cache identities; missing originals never imply a cache miss.
alter table releases add column original_name bytea;
alter table releases add column original_name_key bytea check(octet_length(original_name_key)=32);
create index native_original_name_key on releases(original_name_key,id);
alter table releases add column posted_at bigint;
create index native_posted_at on releases(posted_at desc,id desc);
alter table releases add column posted_clock bigint;
create index native_posted_clock on releases(posted_clock,id);
alter table files add column original_filename bytea;

-- A walked row advances only with its actual leased result. The numeric
-- cursor may point into a gap, including max(id)+1, and is not a release FK.
alter table pp_walks add column native_inflight_release bigint;
create unique index native_walk_single_inflight on pp_walks ((true)) where native_inflight_release is not null;
alter table pp_queue add column native_walk_id bigint references pp_walks(id),
 add column native_walk_before bigint, add column native_walk_redo bigint,
 add check ((native_walk_id is null and native_walk_before is null and native_walk_redo is null)
         or (native_walk_id is not null and native_walk_before is not null and native_walk_redo is not null));
create index native_walk_group_cursor on releases(group_name,id desc);

alter table segment_crc alter column message_id type bytea using convert_to(message_id,'UTF8');

create index media_release_tvdb on media_release(tvdb_id,release_id desc,season,episode,episode_last,kind) where tvdb_id is not null;
create index media_release_tvmaze on media_release(tvmaze_id,release_id desc,season,episode,episode_last,kind) where tvmaze_id is not null;
create index media_release_trakt on media_release(trakt_id,release_id desc,season,episode,episode_last,kind) where trakt_id is not null;
create index media_release_tmdb on media_release(tmdb_id,release_id desc,season,episode,episode_last,kind) where tmdb_id is not null;
create index media_release_imdb on media_release(imdb_id,release_id desc,season,episode,episode_last,kind) where imdb_id is not null;
create index media_release_rage on media_release(tvrage_id,release_id desc,season,episode,episode_last,kind) where tvrage_id is not null;
create index pp_state_net_due on pp_state(net_next_at,release_id) where net_wanted;

-- Complete telemetry inventories are published atomically by the original owner.
create table native_runtime_observations (
 observation_id bigint generated always as identity primary key,
 epoch bytea not null references native_runtime_epochs(epoch),
 catalog_nonce bytea not null references native_catalog_identity(catalog_nonce),
 process_nonce bytea not null check(octet_length(process_nonce)=32),
 stage_instance_nonce bytea check(octet_length(stage_instance_nonce)=32),
 observed_at_ms bigint not null,monotonic_ns bigint not null check(monotonic_ns>=0),
 stage_running boolean not null,enabled_mask bigint,
 net_visits_bits bigint,net_finished_bits bigint,queue_taken_bits bigint,walk_rows_bits bigint,
 check((stage_instance_nonce is null and not stage_running and enabled_mask is null
        and net_visits_bits is null and net_finished_bits is null and queue_taken_bits is null and walk_rows_bits is null)
    or (stage_instance_nonce is not null and enabled_mask is not null and net_visits_bits is not null
        and net_finished_bits is not null and queue_taken_bits is not null and walk_rows_bits is not null))
);
create table native_metric_samples (
 observation_id bigint not null references native_runtime_observations on delete cascade,
 series_key bytea not null check(octet_length(series_key)=32),
 family text not null check(octet_length(family)<=256),labels text not null check(octet_length(labels)<=16384),
 kind smallint not null check(kind between 1 and 3),
 counter_bits bigint,gauge_bits bigint,histogram_count_bits bigint,histogram_sum_bits bigint,
 primary key(observation_id,series_key),
 check((kind=1 and counter_bits is not null and gauge_bits is null and histogram_count_bits is null and histogram_sum_bits is null)
    or (kind=2 and counter_bits is null and gauge_bits is not null and histogram_count_bits is null and histogram_sum_bits is null)
    or (kind=3 and counter_bits is null and gauge_bits is null and histogram_count_bits is not null and histogram_sum_bits is not null))
);
create table native_runtime_observation_current (
 singleton boolean primary key check(singleton),
 observation_id bigint not null references native_runtime_observations
);

-- Representative article counters belong to that same winning subject/MID;
-- these are own-header metadata and confer no cross-release lending proof.
alter table files add column subject_file_index bigint,
 add column subject_file_total bigint,add column subject_total_parts bigint;

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
-- NET work stays on pp_state; independent lane leases cannot overlap BODY or
-- database visits of the same release. IDs/operations survive owner replacement.
alter table releases add column native_work_revision bigint not null default 1 check(native_work_revision>0);
alter table pp_queue add column native_work_revision bigint;
alter table pp_state
 add column native_net_serial bigint not null default 1 check(native_net_serial>0),
 add column native_net_revision bigint,
 add column native_net_rerun bigint,
 add column native_net_before bigint check(native_net_before>=0),
 add column native_net_operation_id bytea check(native_net_operation_id is null or octet_length(native_net_operation_id)=32),
 add column native_net_token bigint,
 add column native_net_until bigint,
 add column native_net_epoch bytea references native_runtime_epochs,
 add check(native_net_epoch is null or native_net_token is not null),
 add check(native_net_before is null or native_net_token is not null);
create function atlas_native_release_input_changed() returns trigger language plpgsql as $$
begin
 if row(NEW.name,NEW.original_name,NEW.original_name_key,NEW.display_name,NEW.name_source,NEW.is_obfuscated,NEW.group_name,NEW.poster,NEW.posted_date,NEW.posted_at,NEW.posted_clock,NEW.size,NEW.parts,NEW.complete,NEW.file_total)
  is distinct from row(OLD.name,OLD.original_name,OLD.original_name_key,OLD.display_name,OLD.name_source,OLD.is_obfuscated,OLD.group_name,OLD.poster,OLD.posted_date,OLD.posted_at,OLD.posted_clock,OLD.size,OLD.parts,OLD.complete,OLD.file_total) then
  NEW.native_work_revision:=OLD.native_work_revision+1;
 end if;
 return NEW;
end $$;
create trigger native_release_input_changed before update of name,original_name,original_name_key,display_name,name_source,is_obfuscated,group_name,poster,posted_date,posted_at,posted_clock,size,parts,complete,file_total on releases
 for each row execute function atlas_native_release_input_changed();

-- One revision bump per affected release/statement, never one per article.
create function atlas_native_files_input_changed() returns trigger language plpgsql as $$
begin
 if TG_OP='INSERT' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new);
 elsif TG_OP='DELETE' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_old);
 else
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new union select release_id from native_old);
 end if;
 return null;
end $$;
create trigger native_input_insert after insert on files referencing new table as native_new
 for each statement execute function atlas_native_files_input_changed();
create trigger native_input_delete after delete on files referencing old table as native_old
 for each statement execute function atlas_native_files_input_changed();
create trigger native_input_update after update on files referencing old table as native_old new table as native_new
 for each statement execute function atlas_native_files_input_changed();

-- One revision bump per affected release/statement, never one per article.
create function atlas_native_segments_input_changed() returns trigger language plpgsql as $$
begin
 if TG_OP='INSERT' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new);
 elsif TG_OP='DELETE' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_old);
 else
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new union select release_id from native_old);
 end if;
 return null;
end $$;
create trigger native_input_insert after insert on segments referencing new table as native_new
 for each statement execute function atlas_native_segments_input_changed();
create trigger native_input_delete after delete on segments referencing old table as native_old
 for each statement execute function atlas_native_segments_input_changed();
create trigger native_input_update after update on segments referencing old table as native_old new table as native_new
 for each statement execute function atlas_native_segments_input_changed();

-- One revision bump per affected release/statement, never one per article.
create function atlas_native_file_timing_input_changed() returns trigger language plpgsql as $$
begin
 if TG_OP='INSERT' then
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_new n on f.id=n.file_id);
 elsif TG_OP='DELETE' then
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_old o on f.id=o.file_id);
 else
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_new n on f.id=n.file_id union select f.release_id from files f join native_old o on f.id=o.file_id);
 end if;
 return null;
end $$;
create trigger native_input_insert after insert on file_timing referencing new table as native_new
 for each statement execute function atlas_native_file_timing_input_changed();
create trigger native_input_delete after delete on file_timing referencing old table as native_old
 for each statement execute function atlas_native_file_timing_input_changed();
create trigger native_input_update after update on file_timing referencing old table as native_old new table as native_new
 for each statement execute function atlas_native_file_timing_input_changed();

drop trigger native_release_search_dirty on releases;
-- Private work-revision changes do not change the actual word document.
-- Every real release field retains the original dirty/catch-up behavior.
create trigger native_release_search_dirty after insert or update of id,group_name,is_collection,identity_key,name,poster,posted_date,
 display_name,is_obfuscated,name_source,size,parts,file_total,complete,original_name,original_name_key,posted_at,posted_clock or delete on releases
 for each row execute function atlas_native_search_dirty();

-- Optional live PAR2 whole-inventory retention. Original offline archives remain separate.
-- Stage-owned successor migration fragment: live optional PAR2 retention only.
-- Never append this to the already-published cache10 migration.
-- A nonempty old target needs separately admitted bounded reconciliation;
-- counter zero must never certify preexisting data. New targets import through
-- these triggers. Offline source backups are outside this live retention tier.
-- Existing facts are frozen for bounded keyset accounting. BUILDING and
-- TRIMMING are maintenance states, never runtime cache admission.
-- Derived accounting has no ext_ prefix: ext dump/restore exports original
-- facts; restored facts rebuild these counters through the same triggers.
create table atlas_par2_cache_usage (
 singleton boolean primary key check(singleton),
 retained_rows bigint not null default 0 check(retained_rows>=0),
 retained_bytes bigint not null default 0 check(retained_bytes>=0),
 pending_xid bigint not null default 0,
 phase text not null default 'building' check(phase in ('building','trimming','ready')),
 scan_relation integer not null default 0 check(scan_relation between 0 and 4),
 after_a bytea,after_b bytea,after_c bytea,
 maintenance_xid bigint not null default 0,
 evict_key bytea,evict_relation integer not null default 0 check(evict_relation between 0 and 4),
 evict_after_a bytea,evict_after_b bytea,evict_after_c bytea,
 evict_total_rows bigint not null default 0,evict_total_bytes bigint not null default 0,
 evict_removed_rows bigint not null default 0,evict_removed_bytes bigint not null default 0,
 evicted_inventories bigint not null default 0,
 evicted_rows bigint not null default 0,evicted_bytes bigint not null default 0,
 check(phase<>'ready' or (scan_relation=4 and evict_key is null))
);
insert into atlas_par2_cache_usage(singleton) values(true);
create sequence atlas_par2_cache_touch;
create table atlas_par2_cache_inventories (
 inventory_key bytea primary key check(octet_length(inventory_key)=32),
 name text collate "C",group_name text collate "C",content_id bytea,
 retained_rows bigint not null check(retained_rows>=0),
 retained_bytes bigint not null check(retained_bytes>=0),
 touched bigint not null
);
create index atlas_par2_cache_oldest on atlas_par2_cache_inventories(touched,inventory_key);

-- Closed derived row locators give bounded indexed eviction even for a
-- million-row inventory, without building a new index over the old fact table.
-- They are populated by admitted bootstrap pages and maintained atomically.
create table atlas_par2_cache_members (
 relation_code integer not null check(relation_code between 0 and 3),
 key_a bytea not null check(octet_length(key_a)=32),
 key_b bytea not null check(octet_length(key_b)=32),
 key_c bytea not null check(octet_length(key_c) in (0,32)),
 inventory_key bytea not null references atlas_par2_cache_inventories(inventory_key),
 retained_bytes bigint not null check(retained_bytes between 2048 and 8388608),
 primary key(relation_code,key_a,key_b,key_c)
);
create index atlas_par2_cache_member_inventory on atlas_par2_cache_members(inventory_key,relation_code,key_a,key_b,key_c);

create function atlas_par2_cache_lock() returns trigger language plpgsql as $$
declare prior record; current_xid bigint:=txid_current();
begin
 select * into strict prior from atlas_par2_cache_usage where singleton for update;
 if TG_TABLE_NAME<>'ext_preserved_cache_rows' and prior.phase<>'ready'
 and not(TG_OP='DELETE' and prior.maintenance_xid=current_xid)
 then raise exception 'PAR2 quota accounting is not ready'; end if;
 if prior.pending_xid<>current_xid then
  update atlas_par2_cache_usage set pending_xid=current_xid where singleton;
 end if;
 return null;
end $$;

create function atlas_par2_cache_delta(n text,g text,c bytea,r bigint,b bigint,code integer,a bytea,z bytea,q bytea) returns void language plpgsql as $$
declare k bytea; changed integer;
begin
 if r not in (-1,1) then raise exception 'invalid PAR2 accounting delta'; end if;
 if r=1 and (b is null or b<2048 or b>8388608)
 then raise exception 'PAR2 accounting row exceeds eviction byte admission'; end if;
 if coalesce(octet_length(n),0)::bigint+coalesce(octet_length(g),0)::bigint+coalesce(octet_length(c),0)::bigint>8388608
 then raise exception 'PAR2 inventory key exceeds admission'; end if;
 k:=sha256(int8send(coalesce(octet_length(n)::bigint,-1))||coalesce(convert_to(n,'UTF8'),''::bytea)
  ||int8send(coalesce(octet_length(g)::bigint,-1))||coalesce(convert_to(g,'UTF8'),''::bytea)
  ||int8send(coalesce(octet_length(c)::bigint,-1))||coalesce(c,''::bytea));
 if r<0 then
  delete from atlas_par2_cache_members member where member.relation_code=code
   and key_a=a and key_b=z and key_c=q and inventory_key=k and retained_bytes=-b;
  get diagnostics changed=row_count;
  if changed<>1 then raise exception 'PAR2 member provenance mismatch'; end if;
 end if;
 update atlas_par2_cache_inventories set retained_rows=retained_rows+r,retained_bytes=retained_bytes+b,
  touched=case when r>0 then nextval('atlas_par2_cache_touch') else touched end
 where inventory_key=k and name is not distinct from n and group_name is not distinct from g
 and content_id is not distinct from c;
 if not found then
  if r<0 or b<0 then raise exception 'missing PAR2 inventory accounting'; end if;
  insert into atlas_par2_cache_inventories(inventory_key,name,group_name,content_id,retained_rows,retained_bytes,touched)
   values(k,n,g,c,r,b,nextval('atlas_par2_cache_touch'));
 end if;
 if r>0 then
  insert into atlas_par2_cache_members values(code,a,z,q,k,b);
 end if;
 update atlas_par2_cache_usage set retained_rows=retained_rows+r,retained_bytes=retained_bytes+b where singleton;
 delete from atlas_par2_cache_inventories where inventory_key=k and retained_rows=0 and retained_bytes=0
 and not exists(select 1 from atlas_par2_cache_usage where evict_key=k);
end $$;

-- Only octet_length metadata is used for payload accounting, including TOAST
-- values. No whole row, JSON tree, hex encoding or row_bytes payload is read.
create function atlas_par2_cache_size(n text,g text,c bytea,extra bigint) returns bigint language sql immutable as $$
 select 2048+2*(coalesce(octet_length(n),0)::bigint+coalesce(octet_length(g),0)::bigint+coalesce(octet_length(c),0)::bigint)+extra
$$;

create function atlas_par2_cache_adjust(n text,g text,c bytea,direction bigint,bytes bigint,code integer,a bytea,z bytea,q bytea) returns void language plpgsql as $$
declare state record;
begin
 select phase,maintenance_xid into strict state from atlas_par2_cache_usage where singleton;
 if state.phase<>'ready' and not(direction<0 and state.maintenance_xid=txid_current())
 then raise exception 'PAR2 quota accounting is not ready'; end if;
 perform atlas_par2_cache_delta(n,g,c,direction,direction*bytes,code,a,z,q);
end $$;

create function atlas_par2_cache_header(r ext_raw_par2,direction bigint) returns void language plpgsql as $$
begin
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.posted),0)::bigint+coalesce(octet_length(r.content_fp),0)::bigint),0,r.natural_name_key,r.natural_group_key,''::bytea);
end $$;

create function atlas_par2_cache_files(r ext_raw_par2_files,direction bigint) returns void language plpgsql as $$
begin
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.natural_file_key),0)::bigint+coalesce(octet_length(r.file_id),0)::bigint+coalesce(octet_length(r.set_id),0)::bigint+coalesce(octet_length(r.md5),0)::bigint+coalesce(octet_length(r.md5_16k),0)::bigint+coalesce(octet_length(r.file_name),0)::bigint+coalesce(octet_length(r.creator),0)::bigint),1,r.natural_name_key,r.natural_group_key,r.natural_file_key);
end $$;

create function atlas_par2_cache_preserved(r ext_preserved_cache_rows,direction bigint) returns void language plpgsql as $$
begin
 if r.family not in ('ext_raw_par2','ext_raw_par2_files') then return; end if;
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.family),0)::bigint+coalesce(octet_length(r.source_digest),0)::bigint+coalesce(octet_length(r.row_digest),0)::bigint+coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.schema_descriptor),0)::bigint+coalesce(octet_length(r.row_bytes),0)::bigint),case when r.family='ext_raw_par2' then 2 else 3 end,r.source_digest,r.row_digest,''::bytea);
end $$;

create function atlas_par2_cache_account() returns trigger language plpgsql as $$
begin
 if TG_TABLE_NAME='ext_preserved_cache_rows' then
  if TG_OP in ('UPDATE','DELETE') then perform atlas_par2_cache_preserved(OLD,-1); end if;
  if TG_OP in ('UPDATE','INSERT') then perform atlas_par2_cache_preserved(NEW,1); end if;
 elsif TG_TABLE_NAME='ext_raw_par2_files' then
  if TG_OP in ('UPDATE','DELETE') then perform atlas_par2_cache_files(OLD,-1); end if;
  if TG_OP in ('UPDATE','INSERT') then perform atlas_par2_cache_files(NEW,1); end if;
 else
  if TG_OP in ('UPDATE','DELETE') then perform atlas_par2_cache_header(OLD,-1); end if;
  if TG_OP in ('UPDATE','INSERT') then perform atlas_par2_cache_header(NEW,1); end if;
 end if;
 return null;
end $$;

-- Closed per-relation metadata expressions, reused by bootstrap and TRUNCATE.
-- Only admitted full keys are fetched, never descriptor or preserved payloads.
create function atlas_par2_cache_extra(relation_name text) returns text language plpgsql immutable as $$
begin
 case relation_name
 when 'ext_raw_par2' then return $expr$coalesce(octet_length(natural_name_key),0)::bigint+coalesce(octet_length(natural_group_key),0)::bigint+coalesce(octet_length(posted),0)::bigint+coalesce(octet_length(content_fp),0)::bigint$expr$;
 when 'ext_raw_par2_files' then return $expr$coalesce(octet_length(natural_name_key),0)::bigint+coalesce(octet_length(natural_group_key),0)::bigint+coalesce(octet_length(natural_file_key),0)::bigint+coalesce(octet_length(file_id),0)::bigint+coalesce(octet_length(set_id),0)::bigint+coalesce(octet_length(md5),0)::bigint+coalesce(octet_length(md5_16k),0)::bigint+coalesce(octet_length(file_name),0)::bigint+coalesce(octet_length(creator),0)::bigint$expr$;
 when 'ext_preserved_cache_rows' then return $expr$coalesce(octet_length(family),0)::bigint+coalesce(octet_length(source_digest),0)::bigint+coalesce(octet_length(row_digest),0)::bigint+coalesce(octet_length(natural_name_key),0)::bigint+coalesce(octet_length(natural_group_key),0)::bigint+coalesce(octet_length(schema_descriptor),0)::bigint+coalesce(octet_length(row_bytes),0)::bigint$expr$;
 else raise exception 'unknown PAR2 accounting relation';end case;
end $$;

-- Restore holds the schema fence and quota lock before TRUNCATE. This cursor
-- fetches one admitted key at a time; it never fetches row_bytes or file_name.
create function atlas_par2_cache_truncate() returns trigger language plpgsql as $$
declare member record;victim record;scan refcursor;low_code integer;high_code integer;
begin
 low_code:=case TG_TABLE_NAME when 'ext_raw_par2' then 0 when 'ext_raw_par2_files' then 1 else 2 end;
 high_code:=case TG_TABLE_NAME when 'ext_preserved_cache_rows' then 3 else low_code end;
 open scan for select * from atlas_par2_cache_members where relation_code between low_code and high_code order by relation_code,key_a,key_b,key_c;
 loop
  fetch scan into member;exit when not found;
  select * into strict victim from atlas_par2_cache_inventories where inventory_key=member.inventory_key;
  perform atlas_par2_cache_adjust(victim.name,victim.group_name,victim.content_id,-1,member.retained_bytes,
   member.relation_code,member.key_a,member.key_b,member.key_c);
 end loop;
 close scan;return null;
end $$;

-- Lower limits are only for stricter admissions/tests; callers cannot raise
-- the existing 250000-row / 256MiB ceilings or the fixed eviction work bound.
create function atlas_par2_cache_evict_page(row_budget integer,byte_budget bigint)
 returns table(deleted_rows bigint,deleted_bytes bigint,finished boolean) language plpgsql as $$
declare state record;member record;changed integer;
begin
 if row_budget<0 or row_budget>256 or byte_budget<0 or byte_budget>8388608
 then raise exception 'invalid PAR2 eviction page admission'; end if;
 deleted_rows:=0;deleted_bytes:=0;finished:=false;
 loop
  select * into strict state from atlas_par2_cache_usage where singleton;
  if state.phase<>'trimming' or state.evict_key is null or state.maintenance_xid<>txid_current()
  then raise exception 'PAR2 eviction is not owned maintenance'; end if;
  if state.evict_relation=4 then finished:=true;exit; end if;
  select * into member from atlas_par2_cache_members where inventory_key=state.evict_key
   and relation_code=state.evict_relation
   and (state.evict_after_a is null or (key_a,key_b,key_c)>(state.evict_after_a,state.evict_after_b,state.evict_after_c))
   order by key_a,key_b,key_c limit 1;
  if not found then
   update atlas_par2_cache_usage set evict_relation=evict_relation+1,evict_after_a=null,evict_after_b=null,evict_after_c=null where singleton;
   continue;
  end if;
  if member.retained_bytes>8388608 then raise exception 'PAR2 eviction row exceeds byte admission'; end if;
  if deleted_rows>=row_budget or deleted_bytes+member.retained_bytes>byte_budget then exit; end if;
  case member.relation_code
  when 0 then delete from ext_raw_par2 where natural_name_key=member.key_a and natural_group_key=member.key_b;
  when 1 then delete from ext_raw_par2_files where natural_name_key=member.key_a and natural_group_key=member.key_b and natural_file_key=member.key_c;
  when 2 then delete from ext_preserved_cache_rows where family='ext_raw_par2' and source_digest=member.key_a and row_digest=member.key_b;
  when 3 then delete from ext_preserved_cache_rows where family='ext_raw_par2_files' and source_digest=member.key_a and row_digest=member.key_b;
  end case;
  get diagnostics changed=row_count;
  if changed<>1 then raise exception 'PAR2 retained member is missing'; end if;
  update atlas_par2_cache_usage set evict_after_a=member.key_a,evict_after_b=member.key_b,evict_after_c=member.key_c,
   evict_removed_rows=evict_removed_rows+1,evict_removed_bytes=evict_removed_bytes+member.retained_bytes where singleton;
  deleted_rows:=deleted_rows+1;deleted_bytes:=deleted_bytes+member.retained_bytes;
 end loop;
 return next;
end $$;

create function atlas_par2_cache_trim(row_limit bigint default 250000,byte_limit bigint default 268435456,
                                     eviction_limit integer default 256)
 returns table(inventories bigint,rows_removed bigint,bytes_removed bigint) language plpgsql as $$
declare u record;victim record;page record;turns integer:=0;spent_rows bigint:=0;spent_bytes bigint:=0;atomic_mode boolean;
begin
 if row_limit<1 or row_limit>250000 or byte_limit<1 or byte_limit>268435456
 or eviction_limit<1 or eviction_limit>256 then raise exception 'invalid PAR2 quota admission'; end if;
 select * into strict u from atlas_par2_cache_usage where singleton for update;
 if u.phase='building' then raise exception 'PAR2 quota source scan is incomplete'; end if;
 atomic_mode:=u.phase='ready';inventories:=0;rows_removed:=0;bytes_removed:=0;
 if atomic_mode and u.evict_key is null and u.retained_rows<=row_limit and u.retained_bytes<=byte_limit then return next;return; end if;
 update atlas_par2_cache_usage set maintenance_xid=txid_current(),phase='trimming' where singleton;
 loop
  select * into strict u from atlas_par2_cache_usage where singleton;
  if u.evict_key is null and u.retained_rows<=row_limit and u.retained_bytes<=byte_limit then exit; end if;
  if turns>=eviction_limit or spent_rows>=256 or spent_bytes>=8388608 then
   if atomic_mode then raise exception 'PAR2 quota eviction work admission exceeded'; end if;
   exit;
  end if;
  if u.evict_key is null then
   select * into strict victim from atlas_par2_cache_inventories where retained_rows>0 order by touched,inventory_key limit 1 for update;
   if atomic_mode and (victim.retained_rows>256-spent_rows or victim.retained_bytes>8388608-spent_bytes)
   then raise exception 'PAR2 victim requires bounded maintenance'; end if;
   update atlas_par2_cache_usage set evict_key=victim.inventory_key,evict_relation=0,
    evict_after_a=null,evict_after_b=null,evict_after_c=null,
    evict_total_rows=victim.retained_rows,evict_total_bytes=victim.retained_bytes,
    evict_removed_rows=0,evict_removed_bytes=0 where singleton;
  end if;
  select * into strict page from atlas_par2_cache_evict_page((256-spent_rows)::integer,8388608-spent_bytes);
  spent_rows:=spent_rows+page.deleted_rows;spent_bytes:=spent_bytes+page.deleted_bytes;
  if not page.finished then
   if atomic_mode then raise exception 'PAR2 victim requires bounded maintenance'; end if;
   exit;
  end if;
  select * into strict u from atlas_par2_cache_usage where singleton;
  select * into strict victim from atlas_par2_cache_inventories where inventory_key=u.evict_key;
  if victim.retained_rows<>0 or victim.retained_bytes<>0 or u.evict_removed_rows<>u.evict_total_rows or u.evict_removed_bytes<>u.evict_total_bytes
  then raise exception 'PAR2 whole-inventory accounting mismatch'; end if;
  delete from atlas_par2_cache_inventories where inventory_key=u.evict_key;
  update atlas_par2_cache_usage set evicted_inventories=evicted_inventories+1,
   evicted_rows=evicted_rows+evict_removed_rows,evicted_bytes=evicted_bytes+evict_removed_bytes,
   evict_key=null,evict_relation=0,evict_after_a=null,evict_after_b=null,evict_after_c=null,
   evict_total_rows=0,evict_total_bytes=0,evict_removed_rows=0,evict_removed_bytes=0 where singleton;
  inventories:=inventories+1;rows_removed:=rows_removed+u.evict_removed_rows;bytes_removed:=bytes_removed+u.evict_removed_bytes;
  turns:=turns+1;
 end loop;
 update atlas_par2_cache_usage set maintenance_xid=0,
  phase=case when evict_key is null and retained_rows<=row_limit and retained_bytes<=byte_limit then 'ready' else 'trimming' end where singleton;
 return next;
end $$;

-- Explicit pre-admission/drained-owner maintenance can quarantine the oldest
-- whole bundle after an atomic writer refused its fixed deletion budget.
-- This creates no readiness: only real four-relation EOF can complete it.
create function atlas_par2_cache_begin_maintenance() returns void language plpgsql as $$
declare state record;victim record;
begin
 select * into strict state from atlas_par2_cache_usage where singleton for update;
 if state.phase<>'ready' then return; end if;
 select * into victim from atlas_par2_cache_inventories where retained_rows>0 order by touched,inventory_key limit 1 for update;
 if not found then return; end if;
 update atlas_par2_cache_usage set phase='trimming',evict_key=victim.inventory_key,evict_relation=0,
  evict_after_a=null,evict_after_b=null,evict_after_c=null,
  evict_total_rows=victim.retained_rows,evict_total_bytes=victim.retained_bytes,
  evict_removed_rows=0,evict_removed_bytes=0 where singleton;
end $$;

-- Each call inspects at most 257 rows / 8MiB on the server and returns only
-- scalar progress. The original schema-fenced owner commits each page. Cache
-- writers cannot mutate the source relations until all four EOFs were seen.
create function atlas_par2_cache_init_page(page_rows integer default 256,page_bytes bigint default 8388608)
 returns void language plpgsql as $$
declare state record; item record; scan refcursor; relation text; predicate text; key_a text;key_b text;key_c text;
 processed integer:=0;bytes_seen bigint:=0;size bigint;size_expr text;reached_eof boolean:=true;
begin
 if page_rows<1 or page_rows>256 or page_bytes<1 or page_bytes>8388608
 then raise exception 'invalid PAR2 initialization admission'; end if;
 select * into strict state from atlas_par2_cache_usage where singleton for update;
 if state.phase='ready' then return; end if;
 if state.phase='trimming' then perform * from atlas_par2_cache_trim();return; end if;
 case state.scan_relation
 when 0 then relation:='ext_raw_par2';predicate:='true';key_a:='natural_name_key';key_b:='natural_group_key';key_c:=$q$ ''::bytea $q$;
 when 1 then relation:='ext_raw_par2_files';predicate:='true';key_a:='natural_name_key';key_b:='natural_group_key';key_c:='natural_file_key';
 when 2 then relation:='ext_preserved_cache_rows';predicate:=$q$ family='ext_raw_par2' $q$;key_a:='source_digest';key_b:='row_digest';key_c:=$q$ ''::bytea $q$;
 when 3 then relation:='ext_preserved_cache_rows';predicate:=$q$ family='ext_raw_par2_files' $q$;key_a:='source_digest';key_b:='row_digest';key_c:=$q$ ''::bytea $q$;
 else raise exception 'invalid PAR2 accounting cursor';end case;
 size_expr:='atlas_par2_cache_size(name,group_name,content_id,'||atlas_par2_cache_extra(relation)||')';
 open scan for execute format('select a,b,c,bytes,case when bytes<=$5 then name end n,case when bytes<=$5 then group_name end g,case when bytes<=$5 then content_id end content from (select %s a,%s b,%s c,%s bytes,name,group_name,content_id from %I where (%s) and ($1::bytea is null or (%s,%s,%s)>($1,$2,$3))) q order by a,b,c limit $4',
  key_a,key_b,key_c,size_expr,relation,predicate,key_a,key_b,key_c)
 using state.after_a,state.after_b,state.after_c,page_rows+1,page_bytes;
 loop
  fetch scan into item;exit when not found;
  size:=item.bytes;
  if processed>=page_rows or bytes_seen+size>page_bytes then
   if processed=0 then raise exception 'PAR2 initialization row exceeds byte admission'; end if;
   reached_eof:=false;exit;
  end if;
  perform atlas_par2_cache_delta(item.n,item.g,item.content,1,size,state.scan_relation,item.a,item.b,item.c);
  processed:=processed+1;bytes_seen:=bytes_seen+size;
  update atlas_par2_cache_usage set after_a=item.a,after_b=item.b,after_c=item.c where singleton;
 end loop;
 close scan;
 if reached_eof then
  update atlas_par2_cache_usage set scan_relation=scan_relation+1,after_a=null,after_b=null,after_c=null,
   phase=case when scan_relation=3 then 'trimming' else phase end where singleton;
 end if;
end $$;

-- Called only after the cache converter/restore has checked every COPY's
-- source count and digest. It never marks data ready: large verified imports
-- commit in TRIMMING and the bounded maintenance API must finish retention.
create function atlas_par2_cache_import_finish() returns void language plpgsql as $$
begin
 perform 1 from atlas_par2_cache_usage where singleton and phase='ready' for update;
 if not found then raise exception 'PAR2 initialization must complete before import'; end if;
 update atlas_par2_cache_usage set phase='trimming' where singleton;
 perform * from atlas_par2_cache_trim();
end $$;

create function atlas_par2_cache_commit() returns trigger language plpgsql as $$
begin
 if (select phase='ready' from atlas_par2_cache_usage where singleton)
 then perform * from atlas_par2_cache_trim(); end if;
 return null;
end $$;
-- Only the first PAR2 statement changes pending_xid. Counter changes and
-- eviction DELETEs retain it, so one event certifies the completed transaction.
create constraint trigger atlas_par2_cache_at_commit after update on atlas_par2_cache_usage
 deferrable initially deferred for each row when (OLD.pending_xid is distinct from NEW.pending_xid)
 execute function atlas_par2_cache_commit();
do $$ declare relation text; begin
 foreach relation in array array['ext_raw_par2','ext_raw_par2_files','ext_preserved_cache_rows'] loop
  execute format('create trigger par2_cache_lock before insert or update or delete or truncate on %I for each statement execute function atlas_par2_cache_lock()',relation);
  execute format('create trigger par2_cache_account after insert or update or delete on %I for each row execute function atlas_par2_cache_account()',relation);
  execute format('create trigger par2_cache_truncate before truncate on %I for each statement execute function atlas_par2_cache_truncate()',relation);
 end loop;
end $$;
revoke all on atlas_par2_cache_usage,atlas_par2_cache_inventories,atlas_par2_cache_members,atlas_par2_cache_touch from public;
revoke all on function atlas_par2_cache_lock(),atlas_par2_cache_delta(text,text,bytea,bigint,bigint,integer,bytea,bytea,bytea),
 atlas_par2_cache_size(text,text,bytea,bigint),atlas_par2_cache_adjust(text,text,bytea,bigint,bigint,integer,bytea,bytea,bytea),
 atlas_par2_cache_header(ext_raw_par2,bigint),atlas_par2_cache_files(ext_raw_par2_files,bigint),
 atlas_par2_cache_preserved(ext_preserved_cache_rows,bigint),atlas_par2_cache_extra(text),atlas_par2_cache_account(),atlas_par2_cache_truncate(),atlas_par2_cache_evict_page(integer,bigint),atlas_par2_cache_trim(bigint,bigint,integer),atlas_par2_cache_begin_maintenance(),
 atlas_par2_cache_init_page(integer,bigint),atlas_par2_cache_import_finish(),atlas_par2_cache_commit() from public;

-- At most five admitted pages during schema installation; empty new targets
-- become ready from actual EOFs, existing targets remain explicitly fenced.
do $$ begin
 for turn in 1..5 loop perform atlas_par2_cache_init_page(); end loop;
end $$;

-- Migration 8 installed this gate on then-existing tables. New worker/result
-- tables participate in the identical unchanged fence and READ COMMITTED gate.
do $$
declare relation record;
begin
 for relation in select c.oid,c.relname from pg_class c join pg_namespace n on n.oid=c.relnamespace
  where n.nspname=current_schema() and c.relkind='r' and c.relname<>'schema_migrations'
    and c.relname not like 'history\_%' escape '\'
    and not exists(select 1 from pg_trigger t where t.tgrelid=c.oid and t.tgname='atlas_history_write_gate')
 loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation.relname);
 end loop;
end $$;


-- PROPOSED SOURCE ONLY: sole migration11 owner reviews/integrates. No DDL executed.
-- Header+rows+head are committed together by ORIGINAL Owner RC after receipt of
-- the checked LIST terminator. There is deliberately no caller readiness flag.
create table native_group_captures (
    id bigint generated always as identity primary key,
    provider_binding bytea not null check(octet_length(provider_binding)=32),
    scope text not null check(scope in('full','pattern')),
    requested_pattern text,
    pattern_key bytea not null check(octet_length(pattern_key)=32),
    captured_at bigint not null,
    input_rows bigint not null check(input_rows>=0),
    accepted_rows bigint not null check(accepted_rows>=0 and accepted_rows<=input_rows),
    source_digest bytea not null check(octet_length(source_digest)=32),
    check((scope='full' and requested_pattern is null) or
          (scope='pattern' and requested_pattern is not null and octet_length(requested_pattern) between 1 and 4096)),
    unique(id,provider_binding,scope,pattern_key)
);
create index native_group_captures_lookup on native_group_captures(provider_binding,scope,captured_at desc,id desc);
create table native_group_capture_rows (
    capture_id bigint not null references native_group_captures(id) on delete cascade,
    ordinal bigint not null check(ordinal>0),
    name text collate "C" not null check(octet_length(name)>0 and octet_length(name)<=512),
    high bigint not null,
    low bigint not null,
    approximate numeric(20,0) generated always as(high::numeric-low::numeric) stored,
    primary key(capture_id,ordinal)
);
create index native_group_rows_order on native_group_capture_rows(capture_id,approximate desc,ordinal);
-- pattern_key = SHA256("atlas.native.group.scope.v1\0" || scope:u8 ||
--                      length:LEu64 || exact_pattern_utf8); full uses scope0/len0,
-- pattern uses scope1/nonempty bytes. Read compares literal pattern as well.
-- No raw pattern is ever indexed; the fixed key cannot alias full inventory.
create table native_group_capture_current (
    provider_binding bytea not null check(octet_length(provider_binding)=32),
    scope text not null check(scope in('full','pattern')),
    pattern_key bytea not null check(octet_length(pattern_key)=32),
    capture_id bigint not null,
    primary key(provider_binding,scope,pattern_key),
    foreign key(capture_id,provider_binding,scope,pattern_key)
        references native_group_captures(id,provider_binding,scope,pattern_key)
);
-- Apply the unchanged schema8 native-write gate to all3 tables. No release FK:
-- this is provider inventory, never proof of a release/header/worker readiness.

do $$
declare relation text;
begin
 foreach relation in array array['native_group_captures','native_group_capture_rows','native_group_capture_current'] loop
  execute format('create trigger atlas_history_write_gate before insert or update or delete or truncate on %I for each statement execute function atlas_native_write_trigger()',relation);
 end loop;
end $$;
