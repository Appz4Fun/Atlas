//! Checked costly-cache ports. Native selection is validated before any miss,
//! negative result or no-op; only an explicitly legacy scope can use SQLite.

use anyhow::{Result, bail};
use super::{ArchiveRecord, Par2Record, ProbeRecord, SavedArchive, SavedArchiveFile, SavedPar2, SavedPrint, SavedProbe, SavedStill};
use crate::pg::cache_actor::{NativeCacheCommand, NativeCacheReply, execute_if_native};
use crate::pg::cache_actor::costly::{Command, Reply};
use crate::pg::natural_cache::Key;

fn execute(command: Command<'_>) -> Result<Option<Reply>> {
    match execute_if_native(NativeCacheCommand::Costly(command))? {
        Some(NativeCacheReply::Costly(reply)) => Ok(Some(reply)),
        Some(_) => bail!("invalid costly cache reply"),
        None => Ok(None),
    }
}

pub fn get_probe_checked(name: &str, group_name: &str, content_id: &[u8], current_parts: i64, current_complete: bool, required_stamp: u64) -> Result<Option<SavedProbe>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Probe { key, parts: current_parts, complete: current_complete, stamp: required_stamp })? {
        Some(Reply::Probe(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_probe(name, group_name, content_id, current_parts, current_complete, required_stamp)),
    }
}

pub fn get_stills_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Vec<SavedStill>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Stills(key))? {
        Some(Reply::Stills(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_stills(name, group_name, content_id)),
    }
}

pub fn get_audio_prints_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Vec<SavedPrint>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::AudioPrints(key))? {
        Some(Reply::AudioPrints(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_audio_prints(name, group_name, content_id)),
    }
}

pub fn get_archive_files_checked(name: &str, group_name: &str, content_id: &[u8], source: i64) -> Result<Vec<SavedArchiveFile>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::ArchiveFiles { key, source })? {
        Some(Reply::ArchiveFiles(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_archive_files(name, group_name, content_id, source)),
    }
}

pub fn get_archive_checked(name: &str, group_name: &str, content_id: &[u8], current_parts: i64, current_complete: bool) -> Result<Option<SavedArchive>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Archive { key, parts: current_parts, complete: current_complete })? {
        Some(Reply::Archive(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_archive(name, group_name, content_id, current_parts, current_complete)),
    }
}

pub fn get_par2_files_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<crate::par2::Inventory>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Par2Files(key))? {
        Some(Reply::Par2Files(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_par2_files(name, group_name, content_id)),
    }
}

pub fn get_par2_checked(name: &str, group_name: &str, content_id: &[u8], current_parts: i64, current_complete: bool) -> Result<Option<SavedPar2>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::Par2 { key, parts: current_parts, complete: current_complete })? {
        Some(Reply::Par2(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_par2(name, group_name, content_id, current_parts, current_complete)),
    }
}

pub fn get_rev_volumes_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<Vec<crate::archive::rev::Volume>>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::RecoveryVolumes(key))? {
        Some(Reply::RecoveryVolumes(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::get_rev_volumes(name, group_name, content_id)),
    }
}

fn nfo_password_entry_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    let key = Key { name, group: group_name, content_id };
    match execute(Command::NfoPassword(key))? {
        Some(Reply::NfoPassword(found)) => Ok(found),
        Some(_) => bail!("invalid costly cache lookup reply"),
        None => Ok(super::raw_nfo_password_entry(name,group_name,content_id)),
    }
}

pub fn get_nfo_password_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    Ok(nfo_password_entry_checked(name,group_name,content_id)?.filter(|password| !password.is_empty()))
}

pub fn has_nfo_password_record_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<bool> {
    Ok(nfo_password_entry_checked(name,group_name,content_id)?.is_some())
}

pub fn get_nested_nzb_password_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<Option<String>> {
    get_nfo_password_checked(&super::nested_nzb_pw_key(name),group_name,content_id)
}

pub fn has_nested_nzb_password_record_checked(name: &str, group_name: &str, content_id: &[u8]) -> Result<bool> {
    has_nfo_password_record_checked(&super::nested_nzb_pw_key(name),group_name,content_id)
}

pub fn put_nested_nzb_password_checked(name: &str, group_name: &str, content_id: &[u8], password: &str) -> Result<()> {
    put_nfo_password_checked(&super::nested_nzb_pw_key(name),group_name,content_id,password)
}

pub fn put_probe_checked(rec: &ProbeRecord<'_>) -> Result<()> {
    match execute(Command::PutProbe(rec))? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_probe(rec); Ok(()) },
    }
}

pub fn put_nfo_password_checked(name: &str, group_name: &str, content_id: &[u8], password: &str) -> Result<()> {
    match execute(Command::PutNfoPassword { key: Key { name, group: group_name, content_id }, password })? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_nfo_password(name, group_name, content_id, password); Ok(()) },
    }
}

pub fn put_stills_checked(name: &str, group_name: &str, content_id: &[u8], stills: &[SavedStill]) -> Result<()> {
    match execute(Command::PutStills { key: Key { name, group: group_name, content_id }, stills })? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_stills(name, group_name, content_id, stills); Ok(()) },
    }
}

pub fn put_audio_prints_checked(name: &str, group_name: &str, content_id: &[u8], prints: &[SavedPrint]) -> Result<()> {
    match execute(Command::PutAudioPrints { key: Key { name, group: group_name, content_id }, prints })? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_audio_prints(name, group_name, content_id, prints); Ok(()) },
    }
}

pub fn put_archive_checked(rec: &ArchiveRecord<'_>) -> Result<()> {
    match execute(Command::PutArchive(rec))? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_archive(rec); Ok(()) },
    }
}

pub fn put_par2_files_checked(name: &str, group_name: &str, content_id: &[u8], inv: &crate::par2::Inventory) -> Result<()> {
    match execute(Command::PutPar2Files { key: Key { name, group: group_name, content_id }, inventory: inv })? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_par2_files(name, group_name, content_id, inv); Ok(()) },
    }
}

pub fn put_par2_checked(rec: &Par2Record<'_>) -> Result<()> {
    match execute(Command::PutPar2(rec))? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_par2(rec); Ok(()) },
    }
}

pub fn put_rev_volumes_checked(name: &str, group_name: &str, content_id: &[u8], volumes: &[crate::archive::rev::Volume]) -> Result<()> {
    match execute(Command::PutRecoveryVolumes { key: Key { name, group: group_name, content_id }, volumes })? {
        Some(Reply::Stored) => Ok(()),
        Some(_) => bail!("invalid costly cache store reply"),
        None => { super::put_rev_volumes(name, group_name, content_id, volumes); Ok(()) },
    }
}
