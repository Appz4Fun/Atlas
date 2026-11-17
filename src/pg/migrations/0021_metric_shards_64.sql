-- release_writer::SHARDS went from 16 to 64 (saves of one busy group queued on its 16 group#shard advisory locks).
-- The catalog metrics rows are kept per (group, writer shard) so that a save only updates its own shard's row; the
-- shard function follows the writer's (`release_writer::shard`: the identity key's first 8 bytes little-endian mod 64,
-- which is its first byte mod 64). A release's shard changes with the modulus, so the rows are rebuilt from releases:
-- left as they were, a delete would take a release off a row it was never counted in. Readers sum a group's shards,
-- and a (group, shard) row exists only while it counts a release, so no reader assumes any fixed set of shards.
lock table native_metric_catalog_groups in exclusive mode;

create or replace function atlas_native_catalog_shard(identity_key bytea) returns smallint language sql immutable as
 $$ select (get_byte(identity_key,0)%64)::smallint $$;

delete from native_metric_catalog_groups;
insert into native_metric_catalog_groups(group_name,shard,total,bytes,complete,nzb_ready,named,still_obfuscated,predb_matched)
select r.group_name, atlas_native_catalog_shard(r.identity_key), count(*), sum(greatest(r.size,0)), sum(r.complete::int),
       sum((r.parts>0)::int), sum((r.display_name is not null)::int), sum((r.is_obfuscated and r.display_name is null)::int),
       sum((exists(select 1 from predb_match p where p.release_id=r.id))::int)
from releases r group by 1,2;
