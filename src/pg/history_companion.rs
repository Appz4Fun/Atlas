//! Receipts issued by the existing main/ext conversion owners, never a second
//! importer. They pair a captured main file and already verified owner results.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RelationReceipt {
    pub table: String,
    pub rows: u64,
    pub digest: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub version: u32,
    pub owner: String,
    pub main_source_digest: [u8; 32],
    pub relations: Vec<RelationReceipt>,
    pub scope: String,
}
pub(crate) struct Witness {
    stamp: std::fs::Metadata,
    pub digest: [u8; 32],
}
impl Witness {
    /// Legacy owners may accept other inputs; only consolidated regular files
    /// can issue a reusable historical pairing. No live source is discovered.
    pub fn capture(path: &Path) -> Result<Option<Self>> {
        if !path.try_exists()? {
            return Ok(None);
        }
        let mut file = std::fs::File::open(path)?;
        let stamp = file.metadata()?;
        if !stamp.is_file() || stamp.len() < 100 {
            return Ok(None);
        }
        let mut header = [0u8; 20];
        file.read_exact(&mut header)?;
        if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
            return Ok(None);
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            if std::path::PathBuf::from(format!("{}{suffix}", path.display())).exists() {
                return Ok(None);
            }
        }
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        let started = std::time::Instant::now();
        let mut bytes = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        while bytes < stamp.len() {
            let n = file.read(&mut buffer[..((stamp.len() - bytes).min(65536) as usize)])?;
            ensure!(n > 0, "SourceMismatch: truncated companion source");
            hash.update(&buffer[..n]);
            bytes += n as u64;
            let target = std::time::Duration::from_secs_f64(bytes as f64 / (32 * 1024 * 1024) as f64);
            if let Some(wait) = target.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        let witness = Self { stamp, digest: hash.finalize().into() };
        witness.check(path)?;
        Ok(Some(witness))
    }
    pub fn check(&self, path: &Path) -> Result<()> {
        let now = std::fs::metadata(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                now.dev() == self.stamp.dev()
                    && now.ino() == self.stamp.ino()
                    && now.len() == self.stamp.len()
                    && now.mtime() == self.stamp.mtime()
                    && now.mtime_nsec() == self.stamp.mtime_nsec()
                    && now.ctime() == self.stamp.ctime()
                    && now.ctime_nsec() == self.stamp.ctime_nsec(),
                "SourceMismatch: companion snapshot changed"
            );
        }
        #[cfg(not(unix))]
        {
            ensure!(
                now.len() == self.stamp.len() && now.modified()? == self.stamp.modified()?,
                "SourceMismatch: companion snapshot changed"
            );
        }
        Ok(())
    }
}
pub(crate) fn relation(table: &str, rows: u64, digest: u128) -> RelationReceipt {
    let mut h = Sha256::new();
    h.update(table.as_bytes());
    h.update(rows.to_le_bytes());
    h.update(digest.to_le_bytes());
    RelationReceipt { table: table.into(), rows, digest: h.finalize().into() }
}
pub(crate) fn store(tx: &mut postgres::Transaction<'_>, receipt: Receipt) -> Result<()> {
    ensure!(receipt.relations.len() <= 64, "InsufficientBudget: companion receipt relation admission");
    let manifest = serde_json::to_vec(&receipt)?;
    ensure!(manifest.len() <= 65536, "InsufficientBudget: companion receipt bytes");
    let digest = Sha256::digest(&manifest).to_vec();
    tx.execute("insert into history_companion_receipts(owner_code,source_digest,receipt_digest,manifest,dirty) values($1,$2,$3,$4,false) on conflict(owner_code) do update set receipt_digest=excluded.receipt_digest,manifest=excluded.manifest,dirty=false where history_companion_receipts.source_digest=excluded.source_digest",&[&receipt.owner,&&receipt.main_source_digest[..],&digest,&manifest])?;
    let row = tx.query_one(
        "select source_digest,receipt_digest,manifest,dirty from history_companion_receipts where owner_code=$1",
        &[&receipt.owner],
    )?;
    ensure!(
        row.get::<_, Vec<u8>>(0) == receipt.main_source_digest
            && row.get::<_, Vec<u8>>(1) == digest
            && row.get::<_, Vec<u8>>(2) == manifest
            && !row.get::<_, bool>(3),
        "SourceMismatch: companion receipt conflict"
    );
    Ok(())
}
pub(crate) fn digest(client: &mut postgres::Client, schema: &str, owner: &str) -> Result<Option<[u8; 32]>> {
    ensure!(super::valid_schema(schema), "DestinationMismatch: receipt schema");
    let row = client.query_opt(
        &format!(
            "select receipt_digest from \"{schema}\".history_companion_receipts where owner_code=$1 and not dirty"
        ),
        &[&owner],
    )?;
    row.map(|r| {
        r.get::<_, Vec<u8>>(0).try_into().map_err(|_| anyhow::anyhow!("CorruptObject: companion digest length"))
    })
    .transpose()
}
pub(crate) fn paired(
    tx: &mut postgres::Transaction<'_>,
    owner: &str,
    expected: [u8; 32],
    main: [u8; 32],
) -> Result<Option<Receipt>> {
    let row = tx.query_opt(
        "select source_digest,receipt_digest,manifest,dirty from history_companion_receipts where owner_code=$1",
        &[&owner],
    )?;
    let Some(row) = row else {
        return Ok(None);
    };
    let bytes: Vec<u8> = row.get(2);
    ensure!(bytes.len() <= 65536, "CorruptObject: companion receipt size");
    ensure!(
        !row.get::<_, bool>(3)
            && row.get::<_, Vec<u8>>(0) == main
            && row.get::<_, Vec<u8>>(1) == expected
            && Sha256::digest(&bytes).as_slice() == expected,
        "SourceMismatch: companion receipt no longer pairs with captured source"
    );
    let receipt: Receipt = serde_json::from_slice(&bytes).context("CorruptObject: companion receipt encoding")?;
    ensure!(
        receipt.version == 1
            && receipt.owner == owner
            && receipt.main_source_digest == main
            && receipt.relations.len() <= 64,
        "SourceMismatch: companion receipt identity"
    );
    Ok(Some(receipt))
}
