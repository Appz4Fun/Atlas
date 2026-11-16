-- One catalog metrics row per group was updated, FOR UPDATE, by every save of the group, and held until it committed:
-- with a group's shards saving side by side (release_writer::SHARDS) they queued on that row instead, and past the
-- helpers' 300 ms lock wait failed and retried. Rows are kept per (group, shard) now, the shard of a release being the
-- writer's (`release_writer::shard`: the first byte of its identity key, mod 16); readers sum a group's shards.
alter table native_metric_catalog_groups add column shard smallint not null default 0;
alter table native_metric_catalog_groups drop constraint native_metric_catalog_groups_pkey;
delete from native_metric_catalog_groups;
alter table native_metric_catalog_groups add primary key (group_name, shard);
insert into native_metric_catalog_groups(group_name,shard,total,bytes,complete,nzb_ready,named,still_obfuscated,predb_matched)
select r.group_name, get_byte(r.identity_key,0)%16, count(*), sum(greatest(r.size,0)), sum(r.complete::int),
       sum((r.parts>0)::int), sum((r.display_name is not null)::int), sum((r.is_obfuscated and r.display_name is null)::int),
       sum((exists(select 1 from predb_match p where p.release_id=r.id))::int)
from releases r group by 1,2;

create function atlas_native_catalog_shard(identity_key bytea) returns smallint language sql immutable as
 $$ select (get_byte(identity_key,0)%16)::smallint $$;

drop function atlas_native_catalog_metric_add(text,bigint,bigint,bigint,bigint,bigint,bigint,bigint);
create function atlas_native_catalog_metric_add(g text,s smallint,t bigint,b bigint,c bigint,p bigint,d bigint,o bigint,m bigint)
returns void language plpgsql as $$
declare now_total bigint;
begin
 if t=0 and b=0 and c=0 and p=0 and d=0 and o=0 and m=0 then return; end if;
 select total into now_total from native_metric_catalog_groups where group_name=g and shard=s for update;
 if not found then
  if t<=0 then raise exception 'native catalog summary is missing' using errcode='55000'; end if;
  insert into native_metric_catalog_groups as old(group_name,shard,total,bytes,complete,nzb_ready,named,still_obfuscated,predb_matched)
  values(g,s,t,b,c,p,d,o,m)
  on conflict(group_name,shard) do update set total=old.total+excluded.total,bytes=old.bytes+excluded.bytes,
   complete=old.complete+excluded.complete,nzb_ready=old.nzb_ready+excluded.nzb_ready,named=old.named+excluded.named,
   still_obfuscated=old.still_obfuscated+excluded.still_obfuscated,predb_matched=old.predb_matched+excluded.predb_matched;
 elsif now_total+t=0 then
  delete from native_metric_catalog_groups where group_name=g and shard=s;
 else
  update native_metric_catalog_groups set total=total+t,bytes=bytes+b,complete=complete+c,nzb_ready=nzb_ready+p,
   named=named+d,still_obfuscated=still_obfuscated+o,predb_matched=predb_matched+m where group_name=g and shard=s;
 end if;
end $$;

create or replace function atlas_native_catalog_metric_rows() returns trigger language plpgsql as $$
declare r record;
begin
 if tg_op='INSERT' then
  for r in
   select x.group_name g,atlas_native_catalog_shard(x.identity_key) s,count(*) t,sum(greatest(x.size,0))::bigint b,
    sum(x.complete::int)::bigint c,sum((x.parts>0)::int)::bigint p,
    sum((x.display_name is not null)::int)::bigint d,sum((x.is_obfuscated and x.display_name is null)::int)::bigint o,
    sum((exists(select 1 from predb_match pm where pm.release_id=x.id))::int)::bigint m
   from native_new x group by 1,2 order by 1,2
  loop
   perform atlas_native_catalog_metric_add(r.g,r.s,r.t,r.b,r.c,r.p,r.d,r.o,r.m);
  end loop;
 else
  for r in
   with changed as (
    select o.id,atlas_native_catalog_shard(o.identity_key) os,o.group_name og,o.size osz,o.complete oc,o.parts op,o.display_name od,o.is_obfuscated oo,
           atlas_native_catalog_shard(w.identity_key) ws,w.group_name wg,w.size wsz,w.complete wc,w.parts wp,w.display_name wd,w.is_obfuscated wo
    from native_old o join native_new w on w.id=o.id
    where row(w.group_name,w.identity_key,w.size,w.complete,w.parts,w.display_name,w.is_obfuscated)
       is distinct from row(o.group_name,o.identity_key,o.size,o.complete,o.parts,o.display_name,o.is_obfuscated)
   ), sides as (
    select og g,os s,-1 n,osz sz,oc c,op p,od d,oo o,id from changed
    union all
    select wg,ws,1,wsz,wc,wp,wd,wo,id from changed
   ), matched as (
    select x.*, exists(select 1 from predb_match pm where pm.release_id=x.id) m from sides x
   )
   select g,s,sum(n)::bigint t,sum(n*greatest(sz,0))::bigint b,sum(n*c::int)::bigint c,sum(n*(p>0)::int)::bigint p,
    sum(n*(d is not null)::int)::bigint d,sum(n*(o and d is null)::int)::bigint o,sum(n*m::int)::bigint m
   from matched group by g,s
   -- one lock order for every writer (removals first only where a total goes down to a group the same statement adds to)
   order by g,s
  loop
   perform atlas_native_catalog_metric_add(r.g,r.s,r.t,r.b,r.c,r.p,r.d,r.o,r.m);
  end loop;
 end if;
 return null;
end $$;

drop trigger native_catalog_metric_delete on releases;
drop function atlas_native_catalog_metrics() cascade;
drop function atlas_native_catalog_metric_delta(text,bigint,bigint,boolean,bigint,text,boolean,boolean);
create function atlas_native_catalog_metric_delta(g text,sh smallint,n bigint,b bigint,c boolean,p bigint,d text,o boolean,m boolean)
returns void language plpgsql as $$
declare remaining bigint;
begin
 if n=1 then
  insert into native_metric_catalog_groups as old(group_name,shard,total,bytes,complete,nzb_ready,named,still_obfuscated,predb_matched)
  values(g,sh,1,greatest(b,0),c::int,(p>0)::int,(d is not null)::int,(o and d is null)::int,m::int)
  on conflict(group_name,shard) do update set total=old.total+1,bytes=old.bytes+excluded.bytes,
   complete=old.complete+excluded.complete,nzb_ready=old.nzb_ready+excluded.nzb_ready,
   named=old.named+excluded.named,still_obfuscated=old.still_obfuscated+excluded.still_obfuscated,predb_matched=old.predb_matched+excluded.predb_matched;
 elsif n=-1 then
  delete from native_metric_catalog_groups where group_name=g and shard=sh and total=1;
  if found then return; end if;
  update native_metric_catalog_groups set total=total-1,bytes=bytes-greatest(b,0),
   complete=complete-c::int,nzb_ready=nzb_ready-(p>0)::int,named=named-(d is not null)::int,
   still_obfuscated=still_obfuscated-(o and d is null)::int,predb_matched=predb_matched-m::int
   where group_name=g and shard=sh returning total into remaining;
  if not found then raise exception 'native catalog summary is missing' using errcode='55000'; end if;
 else raise exception 'invalid native catalog summary delta' using errcode='55000'; end if;
end $$;
create function atlas_native_catalog_metrics() returns trigger language plpgsql as $$
begin
 if tg_op='TRUNCATE' then delete from native_metric_catalog_groups;return null;end if;
 if tg_op='DELETE' then
  perform atlas_native_catalog_metric_delta(old.group_name,atlas_native_catalog_shard(old.identity_key),-1,old.size,old.complete,
   old.parts,old.display_name,old.is_obfuscated,exists(select 1 from predb_match where release_id=old.id));
 end if;
 return old;
end $$;
create trigger native_catalog_metric_delete before delete on releases for each row execute function atlas_native_catalog_metrics();
create trigger native_catalog_metric_truncate after truncate on releases for each statement execute function atlas_native_catalog_metrics();

create or replace function atlas_native_catalog_match_metrics() returns trigger language plpgsql as $$
begin
 if tg_op='TRUNCATE' then update native_metric_catalog_groups set predb_matched=0;return null;end if;
 if tg_op in('UPDATE','DELETE') then
  update native_metric_catalog_groups s set predb_matched=s.predb_matched-1
   from releases r where r.id=old.release_id and s.group_name=r.group_name and s.shard=atlas_native_catalog_shard(r.identity_key);
 end if;
 if tg_op in('INSERT','UPDATE') then
  update native_metric_catalog_groups s set predb_matched=s.predb_matched+1
   from releases r where r.id=new.release_id and s.group_name=r.group_name and s.shard=atlas_native_catalog_shard(r.identity_key);
 end if;
 return coalesce(new,old);
end $$;
