-- The word keeper's acknowledgement deleted many word_queue rows at once while helper writers' release triggers
-- upserted many in their own order, and on prod the two deadlocked (40P01 on the owner session, a paused and reopened
-- indexing attempt each time). Both now lock word_queue rows in release id order: these upserts insert in it, and the
-- acknowledgement locks its rows in it before deleting them (stage_words).
create or replace function atlas_native_known_as_dirty() returns trigger language plpgsql as $$
declare changed bigint;
begin
 if TG_OP='UPDATE' and NEW is not distinct from OLD then return null; end if;
 insert into word_queue(release_id) select release_id from media_release where title_key=
   case when TG_OP='DELETE' then OLD.title_key else NEW.title_key end
 order by release_id
 on conflict(release_id) do update set n=word_queue.n+1;
 get diagnostics changed=ROW_COUNT;
 if changed>0 then perform nextval('native_catalog_revision_seq'); end if;
 return null;
end $$;

create or replace function atlas_native_release_search_dirty_rows() returns trigger language plpgsql as $$
declare marked bigint;
begin
 if tg_op='INSERT' then
  insert into word_queue(release_id) select id from native_new order by id
  on conflict(release_id) do update set n=word_queue.n+1;
 else
  insert into word_queue(release_id)
  select w.id from native_new w join native_old o on o.id=w.id
  where row(w.id,w.group_name,w.is_collection,w.identity_key,w.name,w.poster,w.posted_date,w.display_name,w.is_obfuscated,
            w.name_source,w.size,w.parts,w.file_total,w.complete,w.original_name,w.original_name_key,w.posted_at,w.posted_clock)
     is distinct from
        row(o.id,o.group_name,o.is_collection,o.identity_key,o.name,o.poster,o.posted_date,o.display_name,o.is_obfuscated,
            o.name_source,o.size,o.parts,o.file_total,o.complete,o.original_name,o.original_name_key,o.posted_at,o.posted_clock)
  order by w.id
  on conflict(release_id) do update set n=word_queue.n+1;
 end if;
 get diagnostics marked = row_count;
 if marked>0 then perform nextval('native_catalog_revision_seq'); end if;
 return null;
end $$;
