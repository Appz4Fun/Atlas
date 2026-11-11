//! Private, quota-admitted frozen-frame files. A ready name is created only
//! after fsync; its bytes are still hashed/codec-validated before publication.
use super::history_digest::DigestState;
use crate::history_identity::{ErrorCode, HistoryError};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) const MAX_COMPLETED: usize = 64;
// Logical frame bytes and a conservative allowance for its fixed files and
// directory are charged together, including completed uncommitted frames.
pub(crate) const FRAME_OVERHEAD: u64 = 65536;
pub(crate) struct Existing {
    pub source: i64,
    pub key: i64,
    pub charged_bytes: u64,
}

pub(crate) fn claim_root(root: &Path, scope: [u8; 32]) -> Result<File> {
    let path = root.join("owner.lock");
    let mut file = match open_file(&path, true, true) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
        {
            open_file(&path, true, false)?
        }
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the owned descriptor is live. Nonblocking flock is held by
        // this File through the owner lifetime and released by the OS on loss.
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            HistoryError::new(ErrorCode::LostOwner, "another importer owns the spool root")
        );
    }
    if file.metadata()?.len() == 0 {
        file.write_all(&scope)?;
        file.sync_all()?;
        sync_directory(root)?;
    } else {
        let mut old = Vec::new();
        (&mut file).take(33).read_to_end(&mut old)?;
        ensure!(
            old == scope,
            HistoryError::new(ErrorCode::DestinationMismatch, "spool root belongs to a different destination")
        );
    }
    Ok(file)
}
pub(crate) fn inventory(root: &Path, id: [u8; 32], quota: u64) -> Result<Vec<Existing>> {
    let entries = std::fs::read_dir(root)?.take(MAX_COMPLETED + 3).collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() <= MAX_COMPLETED + 2,
        HistoryError::new(ErrorCode::InsufficientBudget, "spool recovery inventory admission")
    );
    let mut total = 0u64;
    let mut result = Vec::new();
    for entry in entries {
        if entry.file_name() == "owner.lock" {
            continue;
        }
        let path = entry.path();
        ensure!(entry.file_type()?.is_dir(), HistoryError::new(ErrorCode::CorruptObject, "unknown spool root entry"));
        let mut identity = Vec::new();
        open_file(&path.join("identity"), false, false)?.take(49).read_to_end(&mut identity)?;
        ensure!(
            identity.len() == 48 && identity[..32] == id,
            HistoryError::new(ErrorCode::SourceMismatch, "spool recovery capture identity")
        );
        let expected =
            format!("frame-{}", Sha256::digest(&identity).iter().map(|b| format!("{b:02x}")).collect::<String>());
        ensure!(
            entry.file_name() == std::ffi::OsStr::new(&expected),
            HistoryError::new(ErrorCode::CorruptObject, "spool recovery locator")
        );
        let source = i64::from_le_bytes(identity[32..40].try_into()?);
        let key = i64::from_le_bytes(identity[40..48].try_into()?);
        let contents = std::fs::read_dir(&path)?.take(6).collect::<std::io::Result<Vec<_>>>()?;
        ensure!(contents.len() <= 5, HistoryError::new(ErrorCode::CorruptObject, "unknown spool recovery files"));
        let mut bytes = FRAME_OVERHEAD;
        for file in contents {
            let name = file.file_name();
            ensure!(
                ["input.part", "input.ready", "projection.part", "projection.ready", "identity"]
                    .iter()
                    .any(|allowed| name == *allowed)
                    && file.file_type()?.is_file(),
                HistoryError::new(ErrorCode::CorruptObject, "unknown spool recovery file")
            );
            if name != "identity" {
                bytes = bytes.checked_add(file.metadata()?.len()).context("spool inventory overflow")?;
            }
        }
        total = total.checked_add(bytes).context("spool inventory overflow")?;
        ensure!(total <= quota, HistoryError::new(ErrorCode::InsufficientBudget, "retained spool bytes exceed quota"));
        result.push(Existing { source, key, charged_bytes: bytes });
    }
    Ok(result)
}

pub(crate) struct Spool {
    directory: PathBuf,
    input: Option<File>,
    projection: Option<File>,
    pub written: u64,
    pub input_ready: bool,
    pub keep: bool,
    pub quota: u64,
    pub prefix_digest: DigestState,
}
fn open_file(path: &Path, write: bool, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write).create_new(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    ensure!(meta.is_file(), HistoryError::new(ErrorCode::CorruptObject, "spool is not a regular file"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            HistoryError::new(ErrorCode::CorruptObject, "spool file is not private")
        );
    }
    Ok(file)
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
fn available(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        let mut state = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is a live NUL-terminated CString and statvfs writes one
        // correctly aligned statvfs on success, checked before assume_init.
        ensure!(
            unsafe { libc::statvfs(path.as_ptr(), state.as_mut_ptr()) } == 0,
            HistoryError::new(ErrorCode::InsufficientBudget, "cannot inspect spool filesystem")
        );
        let state = unsafe { state.assume_init() };
        #[allow(clippy::unnecessary_cast, reason = "statvfs integer widths vary across Unix targets")]
        let bytes = (state.f_bavail as u64).saturating_mul(state.f_frsize as u64);
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        anyhow::bail!(HistoryError::new(ErrorCode::UnsupportedSchema, "spool filesystem admission is unavailable"))
    }
}
impl Spool {
    #[allow(
        clippy::too_many_arguments,
        reason = "capture identity, durable offset and resource admissions are independent boundary inputs"
    )]
    pub fn open(
        root: &Path,
        fingerprint: [u8; 32],
        source: i64,
        key: i64,
        quota: u64,
        captured: u64,
        length: u64,
        validated_projection: bool,
    ) -> Result<Self> {
        ensure!(
            length.checked_add(FRAME_OVERHEAD).is_some_and(|n| n <= quota) && available(root)? >= quota,
            HistoryError::new(ErrorCode::InsufficientBudget, "frozen spool quota/free space")
        );
        let mut identity = fingerprint.to_vec();
        identity.extend_from_slice(&source.to_le_bytes());
        identity.extend_from_slice(&key.to_le_bytes());
        let name = Sha256::digest(&identity).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let directory = root.join(format!("frame-{name}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => {
                let mut f = open_file(&directory.join("identity"), true, true)?;
                f.write_all(&identity)?;
                f.sync_all()?;
                sync_directory(&directory)?;
                sync_directory(root)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    std::fs::symlink_metadata(&directory)?.is_dir(),
                    HistoryError::new(ErrorCode::CorruptObject, "unknown spool path")
                );
                let mut stored = Vec::new();
                open_file(&directory.join("identity"), false, false)?.take(49).read_to_end(&mut stored)?;
                ensure!(
                    stored == identity,
                    HistoryError::new(ErrorCode::CorruptObject, "spool belongs to another import/object")
                );
            }
            Err(e) => return Err(e.into()),
        }
        let ready = directory.join("input.ready");
        let part = directory.join("input.part");
        let input_ready = captured == length && ready.exists();
        let input = if input_ready {
            let f = open_file(&ready, true, false)?;
            ensure!(f.metadata()?.len() == length, HistoryError::new(ErrorCode::CorruptObject, "ready spool length"));
            f
        } else {
            if ready.exists() {
                std::fs::remove_file(&ready)?;
            }
            let file = if part.exists() { open_file(&part, true, false)? } else { open_file(&part, true, true)? };
            file.set_len(0)?;
            file
        };
        if validated_projection {
            ensure!(
                directory.join("projection.ready").exists(),
                HistoryError::new(ErrorCode::CorruptObject, "validated projection spool missing")
            );
        }
        Ok(Self {
            directory,
            input: Some(input),
            projection: None,
            written: if input_ready { length } else { 0 },
            input_ready,
            keep: validated_projection,
            quota: quota - FRAME_OVERHEAD,
            prefix_digest: Default::default(),
        })
    }
    pub fn append(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        ensure!(
            !self.input_ready
                && offset == self.written
                && offset.checked_add(bytes.len() as u64).is_some_and(|n| n <= self.quota),
            HistoryError::new(ErrorCode::CorruptObject, "spool offset/quota")
        );
        let file = self.input.as_mut().context("spool input closed")?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        self.prefix_digest.update(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
    pub fn sync_page(&mut self) -> Result<()> {
        if let Some(f) = &self.input {
            f.sync_all()?;
        }
        Ok(())
    }
    pub fn finish_input(&mut self, length: u64, digest: [u8; 32]) -> Result<()> {
        if self.input_ready {
            return Ok(());
        }
        ensure!(
            self.written == length && self.prefix_digest.finish()? == digest,
            HistoryError::new(ErrorCode::CorruptObject, "captured spool bytes differ")
        );
        self.sync_page()?;
        std::fs::rename(self.directory.join("input.part"), self.directory.join("input.ready"))?;
        sync_directory(&self.directory)?;
        self.input_ready = true;
        Ok(())
    }
    pub fn input_reader(&self) -> Result<File> {
        ensure!(self.input_ready, HistoryError::new(ErrorCode::IncompleteImport, "spool input is not ready"));
        open_file(&self.directory.join("input.ready"), false, false)
    }
    pub fn projection_writer(&mut self) -> Result<File> {
        let part = self.directory.join("projection.part");
        let ready = self.directory.join("projection.ready");
        if ready.exists() {
            std::fs::remove_file(ready)?;
        }
        if part.exists() {
            std::fs::remove_file(&part)?;
        }
        open_file(&part, true, true)
    }
    pub fn finish_projection(&mut self) -> Result<()> {
        std::fs::rename(self.directory.join("projection.part"), self.directory.join("projection.ready"))?;
        sync_directory(&self.directory)?;
        self.keep = true;
        Ok(())
    }
    pub fn read_projection(&mut self, offset: u64, length: u64, count: usize) -> Result<Vec<u8>> {
        if self.projection.is_none() {
            let f = open_file(&self.directory.join("projection.ready"), false, false)?;
            ensure!(
                f.metadata()?.len() == length,
                HistoryError::new(ErrorCode::CorruptObject, "validated projection length")
            );
            self.projection = Some(f);
        }
        ensure!(
            offset <= length && count as u64 <= length - offset,
            HistoryError::new(ErrorCode::CorruptObject, "projection cursor")
        );
        let file = self.projection.as_mut().expect("projection reader");
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; count];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    /// Close data handles before the page retains only a scalar descriptor.
    pub fn retire(mut self) -> Result<u64> {
        let input = self.input.as_ref().context("missing retired input")?.metadata()?.len();
        let projection = std::fs::metadata(self.directory.join("projection.ready"))?.len();
        let bytes = input
            .checked_add(projection)
            .and_then(|n| n.checked_add(FRAME_OVERHEAD))
            .context("spool quota overflow")?;
        ensure!(
            bytes <= self.quota + FRAME_OVERHEAD,
            HistoryError::new(ErrorCode::InsufficientBudget, "completed frame exceeds its spool admission")
        );
        self.input.take();
        self.projection.take();
        self.keep = true;
        Ok(bytes)
    }
    pub fn cleanup(root: &Path, fingerprint: [u8; 32], source: i64, key: i64) -> Result<()> {
        let mut identity = fingerprint.to_vec();
        identity.extend_from_slice(&source.to_le_bytes());
        identity.extend_from_slice(&key.to_le_bytes());
        let name = Sha256::digest(&identity).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let directory = root.join(format!("frame-{name}"));
        if !directory.exists() {
            return Ok(());
        }
        ensure!(
            std::fs::symlink_metadata(&directory)?.is_dir(),
            HistoryError::new(ErrorCode::CorruptObject, "unknown cleanup path")
        );
        let mut stored = Vec::new();
        open_file(&directory.join("identity"), false, false)?.take(49).read_to_end(&mut stored)?;
        ensure!(stored == identity, HistoryError::new(ErrorCode::CorruptObject, "cleanup ownership receipt"));
        let allowed = ["input.part", "input.ready", "projection.part", "projection.ready", "identity"];
        let entries = std::fs::read_dir(&directory)?.take(6).collect::<std::io::Result<Vec<_>>>()?;
        ensure!(
            entries.len() <= 5
                && entries.iter().all(|e| e.file_name().to_str().is_some_and(|name| allowed.contains(&name))),
            HistoryError::new(ErrorCode::CorruptObject, "unknown spool contents")
        );
        for name in allowed {
            let path = directory.join(name);
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::remove_dir(&directory)?;
        sync_directory(root)?;
        Ok(())
    }
    pub fn remove(&mut self) {
        self.input.take();
        self.projection.take();
        for name in ["input.part", "input.ready", "projection.part", "projection.ready", "identity"] {
            let _ = std::fs::remove_file(self.directory.join(name));
        }
        let _ = std::fs::remove_dir(&self.directory);
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        if !self.keep {
            self.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }
    #[test]
    fn root_claim_is_exclusive_and_destination_bound_across_recovery() {
        let root = root();
        let owner = claim_root(root.path(), [1; 32]).unwrap();
        assert!(claim_root(root.path(), [1; 32]).is_err());
        drop(owner);
        assert!(claim_root(root.path(), [2; 32]).is_err());
        let _resumed = claim_root(root.path(), [1; 32]).unwrap();
    }
    #[test]
    fn retired_frames_close_handles_and_inventory_charges_all_retained_bytes() {
        let root = root();
        let _owner = claim_root(root.path(), [1; 32]).unwrap();
        let original = b"original";
        let projection = b"projection";
        let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, original.len() as u64, false).unwrap();
        spool.append(0, original).unwrap();
        spool.finish_input(original.len() as u64, Sha256::digest(original).into()).unwrap();
        let mut out = spool.projection_writer().unwrap();
        out.write_all(projection).unwrap();
        out.sync_all().unwrap();
        drop(out);
        spool.finish_projection().unwrap();
        spool.read_projection(0, projection.len() as u64, projection.len()).unwrap();
        let charged = spool.retire().unwrap();
        assert_eq!(charged, FRAME_OVERHEAD + (original.len() + projection.len()) as u64);
        let entries = inventory(root.path(), [3; 32], charged).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].charged_bytes, charged);
        assert!(inventory(root.path(), [3; 32], charged - 1).is_err());
        Spool::cleanup(root.path(), [3; 32], 1, 7).unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn cleanup_never_removes_unknown_frame_contents() {
        let root = root();
        let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
        let unknown = spool.directory.join("unowned");
        std::fs::write(&unknown, b"retain").unwrap();
        assert!(Spool::cleanup(root.path(), [3; 32], 1, 7).is_err());
        assert!(unknown.exists());
        spool.keep = true;
    }
}
