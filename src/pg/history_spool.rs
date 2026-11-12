//! Private, quota-admitted frozen-frame files. A ready name is created only
//! after fsync; its bytes are still hashed/codec-validated before publication.
use super::history_digest::DigestState;
use crate::history_identity::{ErrorCode, HistoryError};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) const MAX_COMPLETED: usize = 64;
// Logical frame bytes and a conservative allowance for its fixed files and
// directory/journal are charged together, including uncommitted frame intents.
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupAuthority {
    Identity,
    /// Locator authenticated by the committed import checkpoint. This may
    /// reconcile its empty directory after the final identity unlink.
    CommittedCheckpoint,
}
const FRAME_FILES: [&str; 5] = ["input.part", "input.ready", "projection.part", "projection.ready", "identity"];
const JOURNAL: &str = "frames.journal";
const JOURNAL_NEXT: &str = "frames.journal.next";
const JOURNAL_MAGIC: &[u8; 8] = b"ATSPJ001";
const JOURNAL_HEADER: usize = 40;
const MAX_FRAMES: usize = MAX_COMPLETED + 1;
const MAX_JOURNAL_BYTES: usize = JOURNAL_HEADER + 2 + MAX_FRAMES * 16 + 32;

/// A complete canonical snapshot, independently durable before mkdir and until
/// rmdir is durable. An unpublished staging write never authorizes a frame.
/// Both snapshots together fit within one frame's existing metadata allowance.
struct Journal<'a> {
    root: &'a Path,
    fingerprint: [u8; 32],
    frames: BTreeSet<(i64, i64)>,
}
impl<'a> Journal<'a> {
    fn header(fingerprint: [u8; 32]) -> Vec<u8> {
        let mut bytes = JOURNAL_MAGIC.to_vec();
        bytes.extend_from_slice(&fingerprint);
        bytes
    }
    fn read(root: &Path, name: &str) -> Result<Option<Vec<u8>>> {
        match open_file(&root.join(name), false, false) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_JOURNAL_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= MAX_JOURNAL_BYTES,
                    HistoryError::new(ErrorCode::CorruptObject, "spool journal admission")
                );
                Ok(Some(bytes))
            }
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    fn decode(bytes: &[u8], fingerprint: [u8; 32]) -> Result<BTreeSet<(i64, i64)>> {
        ensure!(
            bytes.len() >= JOURNAL_HEADER + 2 + 32 && bytes.starts_with(&Self::header(fingerprint)),
            HistoryError::new(ErrorCode::SourceMismatch, "spool journal capture identity")
        );
        let count = u16::from_le_bytes(bytes[JOURNAL_HEADER..JOURNAL_HEADER + 2].try_into()?) as usize;
        let end = JOURNAL_HEADER + 2 + count * 16;
        ensure!(
            count <= MAX_FRAMES && bytes.len() == end + 32,
            HistoryError::new(ErrorCode::CorruptObject, "spool journal count/length")
        );
        ensure!(
            Sha256::digest(&bytes[..end]).as_slice() == &bytes[end..],
            HistoryError::new(ErrorCode::CorruptObject, "spool journal digest")
        );
        let mut frames = BTreeSet::new();
        let mut previous = None;
        for row in bytes[JOURNAL_HEADER + 2..end].as_chunks::<16>().0 {
            let locator = (i64::from_le_bytes(row[..8].try_into()?), i64::from_le_bytes(row[8..].try_into()?));
            ensure!(
                previous.is_none_or(|old| old < locator),
                HistoryError::new(ErrorCode::CorruptObject, "spool journal locator ordering")
            );
            frames.insert(locator);
            previous = Some(locator);
        }
        Ok(frames)
    }
    fn load(root: &'a Path, fingerprint: [u8; 32]) -> Result<Self> {
        let canonical = Self::read(root, JOURNAL)?;
        let frames = match &canonical {
            Some(bytes) => Self::decode(bytes, fingerprint)?,
            None => BTreeSet::new(),
        };
        if let Some(bytes) = Self::read(root, JOURNAL_NEXT)? {
            // Only a known private staging file with this exact header prefix
            // can be an interrupted write. The canonical snapshot remains the
            // sole authority; creation cannot precede its fsynced publication.
            let header = Self::header(fingerprint);
            ensure!(
                bytes[..bytes.len().min(JOURNAL_HEADER)] == header[..bytes.len().min(JOURNAL_HEADER)],
                HistoryError::new(ErrorCode::SourceMismatch, "spool staging journal capture identity")
            );
            if bytes.len() >= JOURNAL_HEADER + 2 {
                let count = u16::from_le_bytes(bytes[JOURNAL_HEADER..JOURNAL_HEADER + 2].try_into()?) as usize;
                let length = JOURNAL_HEADER + 2 + count * 16 + 32;
                ensure!(
                    count <= MAX_FRAMES && bytes.len() <= length,
                    HistoryError::new(ErrorCode::CorruptObject, "spool staging journal length")
                );
                if bytes.len() == length {
                    Self::decode(&bytes, fingerprint)?;
                }
            }
            std::fs::remove_file(root.join(JOURNAL_NEXT))?;
            sync_directory(root)?;
        }
        if canonical.is_some() && frames.is_empty() {
            std::fs::remove_file(root.join(JOURNAL))?;
            sync_directory(root)?;
        }
        Ok(Self { root, fingerprint, frames })
    }
    fn publish(&mut self, frames: BTreeSet<(i64, i64)>) -> Result<()> {
        ensure!(
            frames.len() <= MAX_FRAMES,
            HistoryError::new(ErrorCode::InsufficientBudget, "spool journal frame admission")
        );
        let mut bytes = Self::header(self.fingerprint);
        bytes.extend_from_slice(&(frames.len() as u16).to_le_bytes());
        for &(source, key) in &frames {
            bytes.extend_from_slice(&source.to_le_bytes());
            bytes.extend_from_slice(&key.to_le_bytes());
        }
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        let temporary = self.root.join(JOURNAL_NEXT);
        let mut file = open_file(&temporary, true, true)?;
        #[cfg(test)]
        {
            testing::boundary("after_journal_staging_create")?;
            let split = bytes.len() / 2;
            file.write_all(&bytes[..split])?;
            testing::boundary("after_journal_partial_write")?;
            file.write_all(&bytes[split..])?;
            testing::boundary("after_journal_write")?;
        }
        #[cfg(not(test))]
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        testing::boundary("after_journal_staging_sync")?;
        std::fs::rename(&temporary, self.root.join(JOURNAL))?;
        sync_directory(self.root)?;
        self.frames = frames;
        #[cfg(test)]
        testing::boundary("after_journal_publish")?;
        if self.frames.is_empty() {
            std::fs::remove_file(self.root.join(JOURNAL))?;
            #[cfg(test)]
            testing::boundary("after_empty_journal_remove")?;
            sync_directory(self.root)?;
        }
        Ok(())
    }
    fn register(&mut self, source: i64, key: i64) -> Result<()> {
        if !self.frames.contains(&(source, key)) {
            let mut next = self.frames.clone();
            next.insert((source, key));
            self.publish(next)?;
        }
        Ok(())
    }
    fn unregister(&mut self, source: i64, key: i64) -> Result<()> {
        if self.frames.contains(&(source, key)) {
            let mut next = self.frames.clone();
            next.remove(&(source, key));
            self.publish(next)?;
        }
        Ok(())
    }
}
fn frame_identity(fingerprint: [u8; 32], source: i64, key: i64) -> Vec<u8> {
    let mut identity = fingerprint.to_vec();
    identity.extend_from_slice(&source.to_le_bytes());
    identity.extend_from_slice(&key.to_le_bytes());
    identity
}
fn frame_name(identity: &[u8]) -> String {
    format!("frame-{}", Sha256::digest(identity).iter().map(|b| format!("{b:02x}")).collect::<String>())
}
fn frame_contents(directory: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let files = std::fs::read_dir(directory)?.take(FRAME_FILES.len() + 1).collect::<std::io::Result<Vec<_>>>()?;
    ensure!(files.len() <= FRAME_FILES.len(), HistoryError::new(ErrorCode::CorruptObject, "unknown spool contents"));
    for file in &files {
        ensure!(
            file.file_type()?.is_file() && FRAME_FILES.iter().any(|name| file.file_name() == *name),
            HistoryError::new(ErrorCode::CorruptObject, "unknown spool contents")
        );
    }
    Ok(files)
}
fn checked_identity(directory: &Path, identity: &[u8], journal: bool, checkpoint: bool) -> Result<bool> {
    let contents = frame_contents(directory)?;
    match open_file(&directory.join("identity"), false, false) {
        Ok(file) => {
            let mut stored = Vec::new();
            file.take(49).read_to_end(&mut stored)?;
            if stored == identity {
                return Ok(true);
            }
            ensure!(
                journal
                    && stored.len() < 48
                    && identity.starts_with(&stored)
                    && contents.len() == 1
                    && contents[0].file_name() == "identity",
                HistoryError::new(ErrorCode::CorruptObject, "spool identity is not an owned interrupted creation")
            );
            Ok(false)
        }
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            ensure!(
                (journal || checkpoint) && contents.is_empty(),
                HistoryError::new(
                    ErrorCode::CorruptObject,
                    "identity-free spool lacks independent ownership or is not empty"
                )
            );
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
pub(crate) fn inventory(
    root: &Path,
    id: [u8; 32],
    quota: u64,
    checkpoint_cleanups: &[(i64, i64)],
) -> Result<Vec<Existing>> {
    ensure!(
        checkpoint_cleanups.len() <= MAX_COMPLETED + 1,
        HistoryError::new(ErrorCode::CorruptObject, "cleanup receipt admission")
    );
    let journal = Journal::load(root, id)?;
    let mut authenticated: BTreeMap<_, _> = checkpoint_cleanups
        .iter()
        .map(|&(source, key)| (std::ffi::OsString::from(frame_name(&frame_identity(id, source, key))), (source, key)))
        .collect();
    let mut retained: BTreeMap<_, _> = journal.frames.iter().map(|&locator| (locator, FRAME_OVERHEAD)).collect();
    for &(source, key) in &journal.frames {
        authenticated.insert(std::ffi::OsString::from(frame_name(&frame_identity(id, source, key))), (source, key));
    }
    let entries = std::fs::read_dir(root)?.take(MAX_FRAMES + 4).collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() <= MAX_FRAMES + 3,
        HistoryError::new(ErrorCode::InsufficientBudget, "spool recovery inventory admission")
    );
    for entry in entries {
        if ["owner.lock", JOURNAL, JOURNAL_NEXT].iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        let path = entry.path();
        ensure!(entry.file_type()?.is_dir(), HistoryError::new(ErrorCode::CorruptObject, "unknown spool root entry"));
        let contents = frame_contents(&path)?;
        let mut identity = Vec::new();
        let locator = match open_file(&path.join("identity"), false, false) {
            Ok(file) => {
                file.take(49).read_to_end(&mut identity)?;
                if identity.len() == 48 {
                    ensure!(
                        identity[..32] == id,
                        HistoryError::new(ErrorCode::SourceMismatch, "spool recovery capture identity")
                    );
                    ensure!(
                        entry.file_name() == std::ffi::OsStr::new(&frame_name(&identity)),
                        HistoryError::new(ErrorCode::CorruptObject, "spool recovery locator")
                    );
                    (i64::from_le_bytes(identity[32..40].try_into()?), i64::from_le_bytes(identity[40..48].try_into()?))
                } else {
                    let &(source, key) = authenticated
                        .get(&entry.file_name())
                        .filter(|locator| journal.frames.contains(*locator))
                        .context(HistoryError::new(
                            ErrorCode::CorruptObject,
                            "partial identity lacks journal ownership",
                        ))?;
                    checked_identity(&path, &frame_identity(id, source, key), true, false)?;
                    (source, key)
                }
            }
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                ensure!(
                    contents.is_empty(),
                    HistoryError::new(ErrorCode::CorruptObject, "identity-free spool is not empty")
                );
                *authenticated.get(&entry.file_name()).context(HistoryError::new(
                    ErrorCode::CorruptObject,
                    "identity-free spool lacks independent cleanup ownership",
                ))?
            }
            Err(error) => return Err(error),
        };
        let mut bytes = FRAME_OVERHEAD;
        for file in contents {
            if file.file_name() != "identity" {
                bytes = bytes.checked_add(file.metadata()?.len()).context("spool inventory overflow")?;
            }
        }
        retained.insert(locator, bytes);
    }
    ensure!(
        retained.len() <= MAX_FRAMES,
        HistoryError::new(ErrorCode::InsufficientBudget, "retained frame count admission")
    );
    let total = retained
        .values()
        .try_fold(0u64, |total, bytes| total.checked_add(*bytes).context("spool inventory overflow"))?;
    ensure!(total <= quota, HistoryError::new(ErrorCode::InsufficientBudget, "retained spool bytes exceed quota"));
    Ok(retained.into_iter().map(|((source, key), charged_bytes)| Existing { source, key, charged_bytes }).collect())
}

pub(crate) struct Spool {
    directory: PathBuf,
    fingerprint: [u8; 32],
    source: i64,
    key: i64,
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
        let identity = frame_identity(fingerprint, source, key);
        let directory = root.join(frame_name(&identity));
        let mut journal = Journal::load(root, fingerprint)?;
        let complete_identity = match std::fs::symlink_metadata(&directory) {
            Ok(meta) => {
                ensure!(meta.is_dir(), HistoryError::new(ErrorCode::CorruptObject, "unknown spool path"));
                checked_identity(&directory, &identity, journal.frames.contains(&(source, key)), false)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        // The durable intent precedes exposing a directory, including its
        // initially empty or partially written identity states.
        journal.register(source, key)?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => {
                #[cfg(test)]
                testing::boundary("after_frame_directory_create")?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    std::fs::symlink_metadata(&directory)?.is_dir(),
                    HistoryError::new(ErrorCode::CorruptObject, "unknown spool path")
                );
            }
            Err(e) => return Err(e.into()),
        }
        if !complete_identity {
            let path = directory.join("identity");
            let mut file = match open_file(&path, true, false) {
                Ok(file) => file,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    open_file(&path, true, true)?
                }
                Err(error) => return Err(error),
            };
            file.set_len(0)?;
            #[cfg(test)]
            {
                testing::boundary("after_frame_identity_create")?;
                file.write_all(&identity[..24])?;
                testing::boundary("after_frame_identity_partial_write")?;
                file.write_all(&identity[24..])?;
            }
            #[cfg(not(test))]
            file.write_all(&identity)?;
            file.sync_all()?;
            sync_directory(&directory)?;
            sync_directory(root)?;
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
            fingerprint,
            source,
            key,
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
    pub fn cleanup(
        root: &Path,
        fingerprint: [u8; 32],
        source: i64,
        key: i64,
        authority: CleanupAuthority,
    ) -> Result<()> {
        let identity = frame_identity(fingerprint, source, key);
        let directory = root.join(frame_name(&identity));
        let mut journal = Journal::load(root, fingerprint)?;
        let metadata = match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                sync_directory(root)?;
                return journal.unregister(source, key);
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(metadata.is_dir(), HistoryError::new(ErrorCode::CorruptObject, "unknown cleanup path"));
        // Inspect every name before any unlink, even when a checkpoint proves
        // ownership of an empty directory whose identity was already removed.
        checked_identity(
            &directory,
            &identity,
            journal.frames.contains(&(source, key)),
            authority == CleanupAuthority::CommittedCheckpoint,
        )?;
        // Identity-backed legacy frames also acquire durable cleanup authority
        // before any unlink; the receipt outlives the directory's identity file.
        journal.register(source, key)?;
        for name in FRAME_FILES {
            // Persist payload removals before relinquishing the identity. A
            // crash thereafter can leave only an authenticated empty directory.
            if name == "identity" {
                sync_directory(&directory)?;
            }
            let path = directory.join(name);
            if path.exists() {
                std::fs::remove_file(path)?;
                #[cfg(test)]
                testing::boundary(name)?;
            }
        }
        sync_directory(&directory)?;
        #[cfg(test)]
        testing::boundary("before_remove_dir")?;
        std::fs::remove_dir(&directory)?;
        #[cfg(test)]
        testing::boundary("after_frame_directory_remove")?;
        sync_directory(root)?;
        journal.unregister(source, key)
    }
    pub fn remove(&mut self) {
        self.input.take();
        self.projection.take();
        if let Some(root) = self.directory.parent() {
            // A best-effort Drop failure leaves the same durable journal
            // authority for the next fenced owner's bounded recovery pass.
            let _ = Self::cleanup(root, self.fingerprint, self.source, self.key, CleanupAuthority::Identity);
        }
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
pub(crate) mod testing {
    use std::cell::Cell;
    thread_local! { static INTERRUPT: Cell<Option<&'static str>> = const { Cell::new(None) }; }
    pub(crate) struct Fault;
    impl Drop for Fault {
        fn drop(&mut self) {
            INTERRUPT.with(|point| point.set(None));
        }
    }
    pub(crate) fn interrupt_at(point: &'static str) -> Fault {
        INTERRUPT.with(|old| {
            assert!(old.get().is_none());
            old.set(Some(point));
        });
        Fault
    }
    pub(crate) fn boundary(point: &'static str) -> std::io::Result<()> {
        if INTERRUPT.with(|old| {
            if old.get() == Some(point) {
                old.set(None);
                true
            } else {
                false
            }
        }) {
            Err(std::io::Error::other(format!("injected cleanup interruption: {point}")))
        } else {
            Ok(())
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
        let entries = inventory(root.path(), [3; 32], charged, &[]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].charged_bytes, charged);
        assert!(inventory(root.path(), [3; 32], charged - 1, &[]).is_err());
        Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn cleanup_interruption_after_every_unlink_can_reclaim_the_same_owned_frame() {
        for point in
            ["input.part", "input.ready", "projection.part", "projection.ready", "identity", "before_remove_dir"]
        {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
            spool.append(0, b"x").unwrap();
            spool.finish_input(1, Sha256::digest(b"x").into()).unwrap();
            let mut projection = spool.projection_writer().unwrap();
            projection.write_all(b"y").unwrap();
            projection.sync_all().unwrap();
            drop(projection);
            spool.finish_projection().unwrap();
            let directory = spool.directory.clone();
            spool.retire().unwrap();
            // Exercise all fixed unlink names, including stale part files.
            std::fs::write(directory.join("input.part"), b"part").unwrap();
            std::fs::write(directory.join("projection.part"), b"part").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for name in ["input.part", "projection.part"] {
                    std::fs::set_permissions(directory.join(name), std::fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            let fault = testing::interrupt_at(point);
            let error = Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap_err();
            assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error}");
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            let pending = inventory(root.path(), [3; 32], 1024 * 1024, &[(1, 7)])
                .expect("owned interrupted cleanup must remain recoverable");
            assert_eq!(pending.len(), 1);
            assert_eq!((pending[0].source, pending[0].key), (1, 7));
            Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::CommittedCheckpoint).unwrap();
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1, "only root ownership lock remains");
        }
    }

    #[test]
    fn missing_identity_needs_exact_checkpoint_and_unknown_content_stays_untouched() {
        let root = root();
        let _owner = claim_root(root.path(), [1; 32]).unwrap();
        let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
        spool.append(0, b"x").unwrap();
        spool.finish_input(1, Sha256::digest(b"x").into()).unwrap();
        let mut projection = spool.projection_writer().unwrap();
        projection.write_all(b"y").unwrap();
        projection.sync_all().unwrap();
        drop(projection);
        spool.finish_projection().unwrap();
        let directory = spool.directory.clone();
        spool.retire().unwrap();
        let fault = testing::interrupt_at("identity");
        assert!(Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).is_err());
        drop(fault);
        // Model a legacy directory with no independent journal authority.
        // An actual abandoned cleanup now retains that authority instead.
        std::fs::remove_file(root.path().join(JOURNAL)).unwrap();
        sync_directory(root.path()).unwrap();
        assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[]).is_err());
        assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[(1, 8)]).is_err());
        assert_eq!(inventory(root.path(), [3; 32], 1024 * 1024, &[(1, 7)]).unwrap().len(), 1);
        let unknown = directory.join("unowned");
        std::fs::write(&unknown, b"retain").unwrap();
        assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[(1, 7)]).is_err());
        assert!(Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::CommittedCheckpoint).is_err());
        assert_eq!(std::fs::read(unknown).unwrap(), b"retain");
    }

    #[test]
    fn cleanup_never_removes_unknown_frame_contents() {
        let root = root();
        let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
        let unknown = spool.directory.join("unowned");
        std::fs::write(&unknown, b"retain").unwrap();
        assert!(Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).is_err());
        assert!(unknown.exists());
        spool.keep = true;
    }

    #[test]
    fn r1_frame_creation_recovers_without_checkpoint_cleanup() {
        let mut failures = Vec::new();
        for point in
            ["after_frame_directory_create", "after_frame_identity_create", "after_frame_identity_partial_write"]
        {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let fault = testing::interrupt_at(point);
            let error = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false)
                .err()
                .expect("creation interruption must fire");
            assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error:#}");
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            match inventory(root.path(), [3; 32], 1024 * 1024, &[]) {
                Ok(entries) => {
                    assert_eq!(entries.len(), 1, "one owned interrupted frame");
                    assert_eq!((entries[0].source, entries[0].key), (1, 7));
                    Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
                }
                Err(error) => failures.push(format!("{point}: {error:#}")),
            }
        }
        assert!(failures.is_empty(), "same-source creation must recover without a fabricated checkpoint: {failures:?}");
    }

    #[test]
    fn r1_abandoned_cleanup_recovers_without_checkpoint_cleanup() {
        let mut failures = Vec::new();
        for point in ["identity", "before_remove_dir"] {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
            spool.append(0, b"x").unwrap();
            spool.finish_input(1, Sha256::digest(b"x").into()).unwrap();
            let mut projection = spool.projection_writer().unwrap();
            projection.write_all(b"y").unwrap();
            projection.sync_all().unwrap();
            drop(projection);
            spool.finish_projection().unwrap();
            spool.retire().unwrap();
            let fault = testing::interrupt_at(point);
            let error = Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap_err();
            assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error:#}");
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            match inventory(root.path(), [3; 32], 1024 * 1024, &[]) {
                Ok(entries) => {
                    assert_eq!(entries.len(), 1);
                    assert_eq!((entries[0].source, entries[0].key), (1, 7));
                    Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
                }
                Err(error) => failures.push(format!("{point}: {error:#}")),
            }
        }
        assert!(failures.is_empty(), "abandoned cleanup must retain independent ownership: {failures:?}");
    }

    #[test]
    fn r1_unvalidated_drop_recovers_without_checkpoint_cleanup() {
        let mut failures = Vec::new();
        for (ready, point) in [
            (false, "input.part"),
            (true, "input.ready"),
            (true, "projection.part"),
            (false, "identity"),
            (false, "before_remove_dir"),
        ] {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
            spool.append(0, b"x").unwrap();
            if ready {
                spool.finish_input(1, Sha256::digest(b"x").into()).unwrap();
                let mut projection = spool.projection_writer().unwrap();
                projection.write_all(b"y").unwrap();
                projection.sync_all().unwrap();
            }
            assert!(!spool.keep, "exercise the actual unvalidated Drop branch");
            let fault = testing::interrupt_at(point);
            drop(spool);
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            match inventory(root.path(), [3; 32], 1024 * 1024, &[]) {
                Ok(entries) => {
                    assert_eq!(entries.len(), 1);
                    assert_eq!((entries[0].source, entries[0].key), (1, 7));
                    Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
                }
                Err(error) => failures.push(format!("{point}: {error:#}")),
            }
        }
        assert!(failures.is_empty(), "unvalidated Drop must retain recovery ownership: {failures:?}");
    }

    #[test]
    fn r1_journal_publication_interruptions_never_expose_unproven_frames() {
        for (point, owned) in [
            ("after_journal_staging_create", 0),
            ("after_journal_partial_write", 0),
            ("after_journal_write", 0),
            ("after_journal_staging_sync", 0),
            ("after_journal_publish", 1),
        ] {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let fault = testing::interrupt_at(point);
            let error = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false)
                .err()
                .expect("publication interruption must fire");
            assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error:#}");
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            let entries = inventory(root.path(), [3; 32], 1024 * 1024, &[]).unwrap();
            assert_eq!(entries.len(), owned, "only canonical publication is authority: {point}");
            for entry in entries {
                assert_eq!((entry.source, entry.key), (1, 7));
                Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
            }
            let mut resumed = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
            resumed.append(0, b"x").unwrap();
            drop(resumed);
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn r1_journal_retirement_survives_partial_replacement_and_directory_removal() {
        for (point, owned) in [
            ("after_frame_directory_remove", 1),
            ("after_journal_partial_write", 1),
            ("after_journal_publish", 0),
            ("after_empty_journal_remove", 0),
        ] {
            let root = root();
            let owner = claim_root(root.path(), [1; 32]).unwrap();
            let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
            spool.append(0, b"x").unwrap();
            spool.keep = true;
            drop(spool);
            let fault = testing::interrupt_at(point);
            let error = Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap_err();
            assert!(error.to_string().contains("injected cleanup interruption"), "{point}: {error:#}");
            drop(fault);
            drop(owner);
            let _resumed = claim_root(root.path(), [1; 32]).unwrap();
            let entries = inventory(root.path(), [3; 32], 1024 * 1024, &[]).unwrap();
            assert_eq!(entries.len(), owned, "retirement ownership ordering: {point}");
            Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn r1_journal_intents_are_bounded_and_charged_even_without_directories() {
        let root = root();
        let _owner = claim_root(root.path(), [1; 32]).unwrap();
        let mut journal = Journal::load(root.path(), [3; 32]).unwrap();
        for key in 0..65 {
            journal.register(1, key).unwrap();
        }
        let before = std::fs::read(root.path().join(JOURNAL)).unwrap();
        assert!(journal.register(1, 65).is_err(), "a66th pending intent is not admitted");
        assert_eq!(std::fs::read(root.path().join(JOURNAL)).unwrap(), before);
        let entries = inventory(root.path(), [3; 32], 65 * 65536, &[]).unwrap();
        assert_eq!(entries.len(), 65);
        assert!(entries.iter().all(|entry| entry.source == 1 && entry.charged_bytes == 65536));
        assert!(
            inventory(root.path(), [3; 32], 64 * 65536, &[]).is_err(),
            "intent-only ownership still consumes quota"
        );
        for key in 0..65 {
            Spool::cleanup(root.path(), [3; 32], 1, key, CleanupAuthority::Identity).unwrap();
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn r1_wrong_source_corrupt_or_unknown_journals_leave_frame_bytes_untouched() {
        let root = root();
        let _owner = claim_root(root.path(), [1; 32]).unwrap();
        let mut spool = Spool::open(root.path(), [3; 32], 1, 7, 1024 * 1024, 0, 1, false).unwrap();
        spool.append(0, b"x").unwrap();
        let directory = spool.directory.clone();
        spool.keep = true;
        drop(spool);
        let path = root.path().join(JOURNAL);
        let before = std::fs::read(&path).unwrap();
        assert!(inventory(root.path(), [9; 32], 1024 * 1024, &[]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut corrupt = before.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &corrupt).unwrap();
        assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        std::fs::write(&path, &before).unwrap();
        let staging = root.path().join(JOURNAL_NEXT);
        std::fs::write(&staging, b"unowned control content").unwrap();
        assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[]).is_err());
        assert_eq!(std::fs::read(&staging).unwrap(), b"unowned control content");
        assert_eq!(std::fs::read(directory.join("input.part")).unwrap(), b"x");
        std::fs::remove_file(&staging).unwrap();
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), b"unrelated").unwrap();
            std::os::unix::fs::symlink(outside.path(), &staging).unwrap();
            assert!(inventory(root.path(), [3; 32], 1024 * 1024, &[]).is_err());
            assert!(std::fs::symlink_metadata(&staging).unwrap().file_type().is_symlink());
            assert_eq!(std::fs::read(outside.path()).unwrap(), b"unrelated");
            std::fs::remove_file(&staging).unwrap();
        }
        Spool::cleanup(root.path(), [3; 32], 1, 7, CleanupAuthority::Identity).unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
