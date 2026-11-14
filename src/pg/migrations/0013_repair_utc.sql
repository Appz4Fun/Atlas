-- Keep poster-wall-clock metadata unchanged. Existing timing has no proof of
-- its original timezone: never reinterpret it in the installation's timezone
-- or silently rewrite preserved facts to manufacture provider-search bounds.
do $$ begin
 if not pg_try_advisory_xact_lock(hashtext('atlas.indexer'),hashtext(current_schema())) then
  raise exception 'stop the native owner before repair UTC migration' using errcode='55000';
 end if;
end $$;
alter table file_timing
 add column first_utc bigint,
 add column last_utc bigint,
 add column utc_complete boolean not null default false,
 add constraint file_timing_utc_bounds check (
  (first_utc is null)=(last_utc is null)
  and (first_utc is null or first_utc<=last_utc)
  and (not utc_complete or first_utc is not null)
 );
