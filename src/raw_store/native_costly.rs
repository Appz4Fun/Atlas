//! Checked costly-cache ports. Native selection is validated before any miss,
//! negative result or no-op; no absent scope may fall back to another storage backend.

use super::{
    ArchiveRecord, Par2Record, ProbeRecord, SavedArchive, SavedArchiveFile, SavedPar2, SavedPrint, SavedProbe,
    SavedStill,
};
use crate::pg::cache_actor::costly::{Command, Reply};
use crate::pg::cache_actor::{NativeCacheCommand, NativeCacheReply, execute_if_native};
use crate::pg::natural_cache::Key;
use anyhow::{Result, bail};

fn execute(command: Command<'_>) -> Result<Reply> {
    match execute_if_native(NativeCacheCommand::Costly(command))? {
        Some(NativeCacheReply::Costly(reply)) => Ok(reply),
        Some(_) => bail!("invalid costly cache reply"),
        None => bail!("costly cache requires the native owner scope"),
    }
}

pub fn get_probe_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
    required_stamp: u64,
) -> Result<Option<SavedProbe>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Probe { key, parts: current_parts, complete: current_complete, stamp: required_stamp })? {
        Reply::Probe(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_stills_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Vec<SavedStill>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Stills(key))? {
        Reply::Stills(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_audio_prints_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Vec<SavedPrint>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::AudioPrints(key))? {
        Reply::AudioPrints(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_archive_files_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    source: i64,
) -> Result<Vec<SavedArchiveFile>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::ArchiveFiles { key, source })? {
        Reply::ArchiveFiles(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_archive_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
) -> Result<Option<SavedArchive>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Archive { key, parts: current_parts, complete: current_complete })? {
        Reply::Archive(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_par2_files_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
) -> Result<Option<crate::par2::Inventory>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Par2Files(key))? {
        Reply::Par2Files(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_par2_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
) -> Result<Option<SavedPar2>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Par2 { key, parts: current_parts, complete: current_complete })? {
        Reply::Par2(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_rev_volumes_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
) -> Result<Option<Vec<crate::archive::rev::Volume>>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::RecoveryVolumes(key))? {
        Reply::RecoveryVolumes(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

fn nfo_password_entry_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::NfoPassword(key))? {
        Reply::NfoPassword(found) => Ok(found),
        _ => bail!("invalid costly cache lookup reply"),
    }
}

pub fn get_nfo_password_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    Ok(nfo_password_entry_checked(name, group_name, content_id)?.filter(|password| !password.is_empty()))
}

pub fn has_nfo_password_record_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<bool> {
    Ok(nfo_password_entry_checked(name, group_name, content_id)?.is_some())
}

pub fn get_nested_nzb_password_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    get_nfo_password_checked(&super::nested_nzb_pw_key(name), group_name, content_id)
}

pub fn has_nested_nzb_password_record_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<bool> {
    has_nfo_password_record_checked(&super::nested_nzb_pw_key(name), group_name, content_id)
}

pub fn put_nested_nzb_password_checked(name: &str, group_name: &str, content_id: &[u8], password: &str) -> Result<()> {
    put_nfo_password_checked(&super::nested_nzb_pw_key(name), group_name, content_id, password)
}

pub fn put_probe_checked(rec: &ProbeRecord<'_>) -> Result<()> {
    match execute(Command::PutProbe(rec))? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_nfo_password_checked(name: &str, group_name: &str, content_id: &[u8], password: &str) -> Result<()> {
    match execute(Command::PutNfoPassword { key: Key { name, group: group_name, content_id }, password })? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_stills_checked(name: &str, group_name: &str, content_id: &[u8], stills: &[SavedStill]) -> Result<()> {
    match execute(Command::PutStills { key: Key { name, group: group_name, content_id }, stills })? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_audio_prints_checked(name: &str, group_name: &str, content_id: &[u8], prints: &[SavedPrint]) -> Result<()> {
    match execute(Command::PutAudioPrints { key: Key { name, group: group_name, content_id }, prints })? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_archive_checked(rec: &ArchiveRecord<'_>) -> Result<()> {
    match execute(Command::PutArchive(rec))? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_par2_files_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    inv: &crate::par2::Inventory,
) -> Result<()> {
    match execute(Command::PutPar2Files { key: Key { name, group: group_name, content_id }, inventory: inv })? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_par2_checked(rec: &Par2Record<'_>) -> Result<()> {
    match execute(Command::PutPar2(rec))? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}

pub fn put_rev_volumes_checked(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    volumes: &[crate::archive::rev::Volume],
) -> Result<()> {
    match execute(Command::PutRecoveryVolumes { key: Key { name, group: group_name, content_id }, volumes })? {
        Reply::Stored => Ok(()),
        _ => bail!("invalid costly cache store reply"),
    }
}
