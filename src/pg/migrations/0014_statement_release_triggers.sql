-- A save updates hundreds to thousands of releases of one group in one statement. Two row triggers on releases
-- updated the same rows once per release: the catalog revision singleton (search dirty) and the group's catalog
-- metrics row (twice). Every update of one row in a transaction walks that row's versions made earlier in the same
-- transaction, so the cost grew with the square of the batch: on the test server 8000 rows took 703 ms against 26 ms
-- without the trigger, and prod's single owner session spent most of its save time there. Inserts and updates now
-- run once per statement from transition tables; deletes keep their row triggers (the metrics one runs before
-- delete, while `predb_match` still holds the release's rows).

-- Search: one word_queue mark per changed release and one revision bump per statement.
create function atlas_native_release_search_dirty_rows() returns trigger language plpgsql as $$
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
 if marked>0 then update native_catalog_revision set revision=revision+1 where singleton; end if;
 return null;
end $$;

drop trigger native_release_search_dirty on releases;
create trigger native_release_search_dirty after delete on releases
 for each row execute function atlas_native_search_dirty();
create trigger native_release_search_dirty_insert after insert on releases referencing new table as native_new
 for each statement execute function atlas_native_release_search_dirty_rows();
create trigger native_release_search_dirty_update after update on releases referencing old table as native_old new table as native_new
 for each statement execute function atlas_native_release_search_dirty_rows();

-- Catalog metrics: one change per group per statement, the sum of what the row trigger did one release at a time.
create function atlas_native_catalog_metric_add(g text,t bigint,b bigint,c bigint,p bigint,d bigint,o bigint,m bigint)
returns void language plpgsql as $$
declare now_total bigint;
begin
 if t=0 and b=0 and c=0 and p=0 and d=0 and o=0 and m=0 then return; end if;
 select total into now_total from native_metric_catalog_groups where group_name=g for update;
 if not found then
  if t<=0 then raise exception 'native catalog summary is missing' using errcode='55000'; end if;
  insert into native_metric_catalog_groups values(g,t,b,c,p,d,o,m);
 elsif now_total+t=0 then
  -- the positive-total constraint refuses a zero summary row: the group's last release went
  delete from native_metric_catalog_groups where group_name=g;
 else
  update native_metric_catalog_groups set total=total+t,bytes=bytes+b,complete=complete+c,nzb_ready=nzb_ready+p,
   named=named+d,still_obfuscated=still_obfuscated+o,predb_matched=predb_matched+m where group_name=g;
 end if;
end $$;

create function atlas_native_catalog_metric_rows() returns trigger language plpgsql as $$
declare r record;
begin
 if tg_op='INSERT' then
  for r in
   select x.group_name g,count(*) t,sum(greatest(x.size,0))::bigint b,sum(x.complete::int)::bigint c,sum((x.parts>0)::int)::bigint p,
    sum((x.display_name is not null)::int)::bigint d,sum((x.is_obfuscated and x.display_name is null)::int)::bigint o,
    sum((exists(select 1 from predb_match pm where pm.release_id=x.id))::int)::bigint m
   from native_new x group by x.group_name
  loop
   perform atlas_native_catalog_metric_add(r.g,r.t,r.b,r.c,r.p,r.d,r.o,r.m);
  end loop;
 else
  for r in
   with changed as (
    select o.id,o.group_name og,o.size os,o.complete oc,o.parts op,o.display_name od,o.is_obfuscated oo,
           w.group_name wg,w.size ws,w.complete wc,w.parts wp,w.display_name wd,w.is_obfuscated wo
    from native_old o join native_new w on w.id=o.id
    where row(w.group_name,w.size,w.complete,w.parts,w.display_name,w.is_obfuscated)
       is distinct from row(o.group_name,o.size,o.complete,o.parts,o.display_name,o.is_obfuscated)
   ), sides as (
    select og g,-1 n,os s,oc c,op p,od d,oo o,id from changed
    union all
    select wg,1,ws,wc,wp,wd,wo,id from changed
   ), matched as (
    select s.*, exists(select 1 from predb_match pm where pm.release_id=s.id) m from sides s
   )
   select g,sum(n)::bigint t,sum(n*greatest(s,0))::bigint b,sum(n*c::int)::bigint c,sum(n*(p>0)::int)::bigint p,
    sum(n*(d is not null)::int)::bigint d,sum(n*(o and d is null)::int)::bigint o,sum(n*m::int)::bigint m
   from matched group by g
   -- removals first: a release moving groups can empty one group before it adds to another
   order by sum(n)
  loop
   perform atlas_native_catalog_metric_add(r.g,r.t,r.b,r.c,r.p,r.d,r.o,r.m);
  end loop;
 end if;
 return null;
end $$;

drop trigger native_catalog_metric_insert on releases;
drop trigger native_catalog_metric_update on releases;
create trigger native_catalog_metric_insert after insert on releases referencing new table as native_new
 for each statement execute function atlas_native_catalog_metric_rows();
create trigger native_catalog_metric_update after update on releases referencing old table as native_old new table as native_new
 for each statement execute function atlas_native_catalog_metric_rows();
