//! Private, quota-admitted frozen-frame files. A ready name is created only
//! after fsync; its bytes are still hashed/codec-validated before publication.
use super::history_digest::DigestState;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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
    ensure!(meta.is_file(), "CorruptObject: spool is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(meta.permissions().mode() & 0o077 == 0, "CorruptObject: spool file is not private");
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
            "InsufficientBudget: cannot inspect spool filesystem"
        );
        let state = unsafe { state.assume_init() };
        Ok((state.f_bavail as u64).saturating_mul(state.f_frsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        anyhow::bail!("UnsupportedSchema: spool filesystem admission is unavailable")
    }
}
impl Spool {
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
        ensure!(length <= quota && available(root)? >= quota, "InsufficientBudget: frozen spool quota/free space");
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
                ensure!(std::fs::symlink_metadata(&directory)?.is_dir(), "CorruptObject: unknown spool path");
                let mut stored = Vec::new();
                open_file(&directory.join("identity"), false, false)?.take(49).read_to_end(&mut stored)?;
                ensure!(stored == identity, "CorruptObject: spool belongs to another import/object");
            }
            Err(e) => return Err(e.into()),
        }
        let ready = directory.join("input.ready");
        let part = directory.join("input.part");
        let input_ready = captured == length && ready.exists();
        let input = if input_ready {
            let f = open_file(&ready, true, false)?;
            ensure!(f.metadata()?.len() == length, "CorruptObject: ready spool length");
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
            ensure!(directory.join("projection.ready").exists(), "CorruptObject: validated projection spool missing");
        }
        Ok(Self {
            directory,
            input: Some(input),
            projection: None,
            written: if input_ready { length } else { 0 },
            input_ready,
            keep: validated_projection,
            quota,
            prefix_digest: Default::default(),
        })
    }
    pub fn append(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        ensure!(
            !self.input_ready
                && offset == self.written
                && offset.checked_add(bytes.len() as u64).is_some_and(|n| n <= self.quota),
            "CorruptObject: spool offset/quota"
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
            "CorruptObject: captured spool bytes differ"
        );
        self.sync_page()?;
        std::fs::rename(self.directory.join("input.part"), self.directory.join("input.ready"))?;
        sync_directory(&self.directory)?;
        self.input_ready = true;
        Ok(())
    }
    pub fn input_reader(&self) -> Result<File> {
        ensure!(self.input_ready, "IncompleteImport: spool input is not ready");
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
            ensure!(f.metadata()?.len() == length, "CorruptObject: validated projection length");
            self.projection = Some(f);
        }
        ensure!(offset <= length && count as u64 <= length - offset, "CorruptObject: projection cursor");
        let file = self.projection.as_mut().expect("projection reader");
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; count];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
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
