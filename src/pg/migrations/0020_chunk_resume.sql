-- Where a day chunk's read got to on the server that claimed it: the first article number not yet saved, every one
-- from the chunk's start below it saved. A chunk given back part way (a stop, a paused attempt, a failure) is read
-- from there when the same server claims it again, not from its start: on prod ~87% of the articles saves looked up
-- were saved already, each paused attempt dropping every chunk under way.
alter table backfill_chunks add column if not exists resume_host text;
alter table backfill_chunks add column if not exists resume_at bigint;
