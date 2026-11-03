//! What long running commands share.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

pub const COMMITS_PER_CHECKPOINT: u32 = 64;
pub const WAL_LIMIT: u64 = 4 << 30;

/// Call every COMMITS_PER_CHECKPOINT commits: `pragma wal_checkpoint(restart)`, up to 8 tries
/// 250 ms apart. If a reader holds it back and the -wal file is over `limit`: an error. The
/// caller's cursor is committed, soo a run resumes.
pub fn checkpoint_or_stop(conn: &Connection, path: &Path, limit: u64) -> anyhow::Result<()> {
    conn.busy_timeout(Duration::ZERO)?;
    let mut held_back = true;
    for _ in 0..8 {
        let busy: i64 = conn.query_row("pragma wal_checkpoint(restart)", [], |r| r.get(0)).unwrap_or(1);
        if busy == 0 {
            held_back = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    conn.busy_timeout(Duration::from_secs(30))?;
    let wal = std::fs::metadata(format!("{}-wal", path.display())).map(|m| m.len()).unwrap_or(0);
    if held_back && wal > limit {
        anyhow::bail!("a reader is holding the WAL; close the API or the menu and run this again");
    }
    Ok(())
}
