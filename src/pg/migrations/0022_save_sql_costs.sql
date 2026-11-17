-- Cheaper triggers and better plans for the release writer's save statements.
--
-- native_predb_name_update ran atlas_predb_name_dirty (a predb_name_revision bump and a predb_name_pending row)
-- for every row an UPDATE OF name/original_name/display_name/name_source touched, also when the value stayed the
-- same; the release writer's fold sets display_name on every release it updates. The WHEN clause compares exactly
-- the columns the trigger is declared on, so it fires only when one of them really changes (the function itself
-- reads only NEW.id and TG_OP).
drop trigger native_predb_name_update on releases;
create trigger native_predb_name_update after update of name,original_name,display_name,name_source on releases
 for each row
 when (old.name is distinct from new.name or old.original_name is distinct from new.original_name
       or old.display_name is distinct from new.display_name or old.name_source is distinct from new.name_source)
 execute function atlas_predb_name_dirty();

-- atlas_predb_inner_changed only takes nextval('predb_name_revision') and reads neither NEW nor OLD. Its one reader
-- (service_cache/predb_late) only asks whether the sequence's last_value moved, so one bump per statement says the
-- same as one per row. A statement that changes no row now bumps too, which only makes the late scan start over.
drop trigger native_predb_release_evidence on releases;
create trigger native_predb_release_evidence after update of size on releases
 for each statement execute function atlas_predb_inner_changed();
drop trigger native_predb_file_evidence on files;
create trigger native_predb_file_evidence after insert or update or delete on files
 for each statement execute function atlas_predb_inner_changed();

-- files.release_id: ANALYZE's sample put its distinct count at ~485k against ~36M files (about 0.77 per row), so
-- the planner expected thousands of files per release and the writer's release fold took a hash join over all of
-- files (and JIT) on every call. A negative n_distinct is a fraction of the rows, so it stays right as files grows.
-- It takes effect at the next ANALYZE of the column; one of a single column reads a fixed-size sample
-- (300 x default_statistics_target rows), not the table, so it is run here.
alter table files alter column release_id set (n_distinct = -0.77);
analyze files (release_id);
