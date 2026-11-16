-- Helper writer sessions. Every native mutation took a shared advisory lock on `atlas.indexer`, which the owner
-- session holds exclusively, so only the owner could write: on prod every save queued on that one session, which
-- spent 84% of its save time waiting on PostgreSQL while the server had cores to spare. A session that names the
-- current owner epoch (`atlas.helper_epoch`) now writes beside it: once per transaction it takes the shared advisory
-- lock `atlas.indexer.helpers` (a new owner takes it exclusively before it registers, `stage::open_epoch`, so it
-- waits for this transaction to end) and checks that the epoch is active and its owner backend alive and still
-- holding the exclusive lock. Without the setting the guard is what it was. (A lock rather than a row FOR SHARE: every
-- concurrent save sharing one row would make a MultiXact each time.)
create or replace function atlas_native_write_guard(p_schema text) returns void language plpgsql volatile
security definer set search_path to 'pg_catalog' as $$
declare blocked boolean; helper text; alive boolean;
begin
    if current_setting('transaction_isolation') <> 'read committed' then
        raise exception 'native mutation requires READ COMMITTED isolation' using errcode='0A000';
    end if;
    execute format('select exists(select 1 from %I.history_imports)',p_schema) into blocked;
    if blocked then raise exception 'historical import blocks native mutations' using errcode='55000'; end if;
    helper:=current_setting('atlas.helper_epoch',true);
    if helper is null or helper='' then
        perform pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer'),pg_catalog.hashtext(p_schema));
    elsif coalesce(current_setting('atlas.helper_fenced',true),'')<>pg_catalog.txid_current()::text then
        perform pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer.helpers'),pg_catalog.hashtext(p_schema));
        perform pg_catalog.pg_stat_clear_snapshot();
        execute format('select exists(select 1 from %1$I.native_runtime_current c join %1$I.native_runtime_epochs e on e.epoch=c.epoch
            join pg_catalog.pg_stat_activity a on a.pid=e.backend_pid and a.backend_start=e.backend_start
            where c.singleton and e.epoch=pg_catalog.decode($1,''hex'') and e.state=''active''
            and exists(select 1 from pg_catalog.pg_locks l where l.pid=a.pid and l.locktype=''advisory'' and l.mode=''ExclusiveLock''
                and l.granted and l.database=a.datid
                and l.classid=(pg_catalog.hashtext(''atlas.indexer'')::bigint & 4294967295)::oid
                and l.objid=(pg_catalog.hashtext($2)::bigint & 4294967295)::oid and l.objsubid=2))',p_schema)
            using helper,p_schema into alive;
        if not alive then
            raise exception 'native helper writer: its owner epoch ended' using errcode='55000';
        end if;
        perform pg_catalog.set_config('atlas.helper_fenced',pg_catalog.txid_current()::text,true);
    end if;
    execute format('select exists(select 1 from %I.history_imports)',p_schema) into blocked;
    if blocked then raise exception 'historical import blocks native mutations' using errcode='55000'; end if;
end $$;

-- The catalog revision was one row every search-dirty trigger updated: concurrent saves would queue on its row lock
-- until each committed. It only ever goes up and nothing compares it to an exact value, so it is a sequence now.
create sequence native_catalog_revision_seq;
select setval('native_catalog_revision_seq', greatest((select revision from native_catalog_revision where singleton), 1));

create or replace function atlas_native_search_dirty() returns trigger language plpgsql as $$
declare rid bigint;
begin
 if TG_OP='UPDATE' and NEW is not distinct from OLD then return null; end if;
 if TG_TABLE_NAME='releases' then
   rid:=case when TG_OP='DELETE' then OLD.id else NEW.id end;
 else
   rid:=case when TG_OP='DELETE' then OLD.release_id else NEW.release_id end;
 end if;
 insert into word_queue(release_id) values(rid) on conflict(release_id) do update set n=word_queue.n+1;
 perform nextval('native_catalog_revision_seq');
 return null;
end $$;

create or replace function atlas_native_known_as_dirty() returns trigger language plpgsql as $$
declare changed bigint;
begin
 if TG_OP='UPDATE' and NEW is not distinct from OLD then return null; end if;
 insert into word_queue(release_id) select release_id from media_release where title_key=
   case when TG_OP='DELETE' then OLD.title_key else NEW.title_key end
 on conflict(release_id) do update set n=word_queue.n+1;
 get diagnostics changed=ROW_COUNT;
 if changed>0 then perform nextval('native_catalog_revision_seq'); end if;
 return null;
end $$;

create or replace function atlas_native_release_search_dirty_rows() returns trigger language plpgsql as $$
declare marked bigint;
begin
 if tg_op='INSERT' then
  insert into word_queue(release_id) select id from native_new
  on conflict(release_id) do update set n=word_queue.n+1;
 else
  insert into word_queue(release_id)
  select w.id from native_new w join native_old o on o.id=w.id
  where row(w.id,w.group_name,w.is_collection,w.identity_key,w.name,w.poster,w.posted_date,w.display_name,w.is_obfuscated,
            w.name_source,w.size,w.parts,w.file_total,w.complete,w.original_name,w.original_name_key,w.posted_at,w.posted_clock)
     is distinct from
        row(o.id,o.group_name,o.is_collection,o.identity_key,o.name,o.poster,o.posted_date,o.display_name,o.is_obfuscated,
            o.name_source,o.size,o.parts,o.file_total,o.complete,o.original_name,o.original_name_key,o.posted_at,o.posted_clock)
  on conflict(release_id) do update set n=word_queue.n+1;
 end if;
 get diagnostics marked = row_count;
 if marked>0 then perform nextval('native_catalog_revision_seq'); end if;
 return null;
end $$;
