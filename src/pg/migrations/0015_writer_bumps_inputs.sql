-- A save bumped native_work_revision on the same releases four or five times: the statement triggers on files
-- (insert, update) and on file_timing (insert, update) each updated every release they touched, and the writer then
-- bumped all of them once more itself. Every bump is a full update of those release rows. While the writer has
-- `atlas.writer_bumps_inputs` on (transaction-local, only around its own file writes in `flush_local`) it bumps
-- every release whose files or timing it changes, and these triggers leave it to it.
create or replace function atlas_native_files_input_changed() returns trigger language plpgsql as $$
begin
 if current_setting('atlas.writer_bumps_inputs', true) = 'on' then return null; end if;
 if TG_OP='INSERT' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new);
 elsif TG_OP='DELETE' then
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_old);
 else
  update releases set native_work_revision=native_work_revision+1 where id in (select release_id from native_new union select release_id from native_old);
 end if;
 return null;
end $$;

create or replace function atlas_native_file_timing_input_changed() returns trigger language plpgsql as $$
begin
 if current_setting('atlas.writer_bumps_inputs', true) = 'on' then return null; end if;
 if TG_OP='INSERT' then
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_new n on f.id=n.file_id);
 elsif TG_OP='DELETE' then
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_old o on f.id=o.file_id);
 else
  update releases set native_work_revision=native_work_revision+1 where id in (select f.release_id from files f join native_new n on f.id=n.file_id union select f.release_id from files f join native_old o on f.id=o.file_id);
 end if;
 return null;
end $$;
