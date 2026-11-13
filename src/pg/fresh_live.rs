//! Read-only observations of the original native owner session. A persisted
//! active epoch is insufficient: the backend/start and exclusive schema fence
//! must still exist. This type never mints writer or startup authority.
use anyhow::{Result,Context,ensure};
use postgres::GenericClient;
use super::stage::CatalogIdentity;

#[derive(PartialEq,Eq)]
pub(crate) struct RuntimeObservation {epoch:[u8;32],catalog:[u8;32],pid:i32,started:String,code:[u8;32]}
impl RuntimeObservation {
    pub(crate) fn epoch(&self)->&[u8;32]{&self.epoch}
    pub(crate) fn read_in(client:&mut impl GenericClient)->Result<Option<Self>> {
        let catalog=CatalogIdentity::read_in(client)?;
        client.query_one("select pg_stat_clear_snapshot()",&[])?;
        let row=client.query_opt("select e.epoch,e.backend_pid,to_char(e.backend_start at time zone 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),e.code_digest
            from native_runtime_current c join native_runtime_epochs e on e.epoch=c.epoch
            join pg_stat_activity a on a.pid=e.backend_pid and a.backend_start=e.backend_start and a.usesysid=e.role_oid
            join pg_namespace n on n.nspname=current_schema() and n.oid=e.schema_oid and n.nspowner=e.schema_owner_oid
            join pg_database d on d.datname=current_database() and d.oid=e.database_oid and a.datid=d.oid
            join pg_class m on m.relnamespace=n.oid and m.relname='schema_migrations' and m.oid=e.migrations_oid
            where c.singleton and e.state='active' and e.catalog_nonce=$1 and e.schema_version=(select max(version) from schema_migrations)
            and exists(select 1 from pg_locks l where l.pid=a.pid and l.database=d.oid and l.locktype='advisory'
                and l.mode='ExclusiveLock' and l.granted and l.classid=(hashtext('atlas.indexer')::bigint & 4294967295)::oid
                and l.objid=(hashtext(current_schema())::bigint & 4294967295)::oid and l.objsubid=2)",&[&&catalog.nonce()[..]])?;
        row.map(|row|->Result<Self>{
            let fixed=|at|->Result<[u8;32]>{row.try_get::<_,Vec<u8>>(at)?.try_into().map_err(|_|anyhow::anyhow!("native runtime digest extent"))};
            let epoch=fixed(0)?;let code=fixed(3)?;ensure!(epoch!=[0;32]&&code!=[0;32],"invalid native runtime digest");
            Ok(Self {epoch,catalog:*catalog.nonce(),pid:row.try_get(1)?,started:row.try_get(2)?,code})
        }).transpose()
    }
    pub(crate) fn verify_after(&self,client:&mut impl GenericClient)->Result<()> {
        let current=Self::read_in(client)?.context("native owner session ended during read")?;
        ensure!(*self==current,"native owner changed during read");Ok(())
    }
}
