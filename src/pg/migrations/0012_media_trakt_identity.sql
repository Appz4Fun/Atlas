-- Add an exact natural-identity lookup without changing cached data or rows.
-- Every nullable prefix column is constrained by equality or IS NULL; the
-- final Trakt key supports bounded forward/backward endpoint probes.
do $$ begin
 if not pg_try_advisory_xact_lock(hashtext('atlas.indexer'),hashtext(current_schema())) then
  raise exception 'stop the native owner before media identity migration' using errcode='55000';
 end if;
end $$;
create index media_release_details_trakt_identity on media_release
 (kind,tmdb_id,imdb_id,tvdb_id,tvmaze_id,tvrage_id,trakt_id) where trakt_id is not null;
