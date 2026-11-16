-- Saves of one group now run side by side, one per shard of it (release_writer::SHARDS). Two of them creating a
-- group's catalog metrics row at once both found it missing and both inserted it: the second hit the primary key. The
-- insert now adds to a row another transaction created meanwhile, as the row trigger's upsert did before 0014.
create or replace function atlas_native_catalog_metric_add(g text,t bigint,b bigint,c bigint,p bigint,d bigint,o bigint,m bigint)
returns void language plpgsql as $$
declare now_total bigint;
begin
 if t=0 and b=0 and c=0 and p=0 and d=0 and o=0 and m=0 then return; end if;
 select total into now_total from native_metric_catalog_groups where group_name=g for update;
 if not found then
  if t<=0 then raise exception 'native catalog summary is missing' using errcode='55000'; end if;
  insert into native_metric_catalog_groups as old values(g,t,b,c,p,d,o,m)
  on conflict(group_name) do update set total=old.total+excluded.total,bytes=old.bytes+excluded.bytes,
   complete=old.complete+excluded.complete,nzb_ready=old.nzb_ready+excluded.nzb_ready,named=old.named+excluded.named,
   still_obfuscated=old.still_obfuscated+excluded.still_obfuscated,predb_matched=old.predb_matched+excluded.predb_matched;
 elsif now_total+t=0 then
  -- the positive-total constraint refuses a zero summary row: the group's last release went
  delete from native_metric_catalog_groups where group_name=g;
 else
  update native_metric_catalog_groups set total=total+t,bytes=bytes+b,complete=complete+c,nzb_ready=nzb_ready+p,
   named=named+d,still_obfuscated=still_obfuscated+o,predb_matched=predb_matched+m where group_name=g;
 end if;
end $$;
