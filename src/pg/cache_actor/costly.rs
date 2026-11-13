//! Closed commands for preserved costly facts, on the active cache transaction.

use anyhow::Result;
use super::super::natural_cache::{self as cache, Budget, Key};
use crate::raw_store::{ArchiveRecord, Par2Record, ProbeRecord, SavedArchive, SavedArchiveFile, SavedPar2, SavedPrint, SavedProbe, SavedStill};

// Admission includes row/field envelopes and simultaneous decoded results.
const BUDGET: Budget = Budget { bytes: 16 * 1024 * 1024, rows: 10_000 };

pub(crate) enum Command<'a> {
    Probe { key: Key<'a>, parts: i64, complete: bool, stamp: u64 },
    NfoPassword(Key<'a>),
    Stills(Key<'a>),
    AudioPrints(Key<'a>),
    ArchiveFiles { key: Key<'a>, source: i64 },
    Archive { key: Key<'a>, parts: i64, complete: bool },
    Par2Files(Key<'a>),
    Par2 { key: Key<'a>, parts: i64, complete: bool },
    RecoveryVolumes(Key<'a>),
    PutProbe(&'a ProbeRecord<'a>),
    PutNfoPassword { key: Key<'a>, password: &'a str },
    PutStills { key: Key<'a>, stills: &'a [SavedStill] },
    PutAudioPrints { key: Key<'a>, prints: &'a [SavedPrint] },
    PutArchive(&'a ArchiveRecord<'a>),
    PutPar2Files { key: Key<'a>, inventory: &'a crate::par2::Inventory },
    PutPar2(&'a Par2Record<'a>),
    PutRecoveryVolumes { key: Key<'a>, volumes: &'a [crate::archive::rev::Volume] },
}

pub(crate) enum Reply {
    Probe(Option<SavedProbe>),
    NfoPassword(Option<String>),
    Stills(Vec<SavedStill>),
    AudioPrints(Vec<SavedPrint>),
    ArchiveFiles(Vec<SavedArchiveFile>),
    Archive(Option<SavedArchive>),
    Par2Files(Option<crate::par2::Inventory>),
    Par2(Option<SavedPar2>),
    RecoveryVolumes(Option<Vec<crate::archive::rev::Volume>>),
    Stored,
}

pub(crate) fn execute(tx: &mut postgres::Transaction<'_>, command: Command<'_>) -> Result<Reply> {
    use Command::*;
    Ok(match command {
        Probe { key, parts, complete, stamp } => Reply::Probe(cache::probe(tx,key,parts,complete,stamp,BUDGET)?),
        NfoPassword(key) => Reply::NfoPassword(cache::nfo_password(tx,key,BUDGET)?),
        Stills(key) => Reply::Stills(cache::stills(tx,key,BUDGET)?),
        AudioPrints(key) => Reply::AudioPrints(cache::audio_prints(tx,key,BUDGET)?),
        ArchiveFiles { key, source } => Reply::ArchiveFiles(cache::archive_files(tx,key,source,BUDGET)?),
        Archive { key, parts, complete } => Reply::Archive(cache::archive(tx,key,parts,complete,BUDGET)?),
        Par2Files(key) => Reply::Par2Files(cache::par2_files(tx,key,BUDGET)?),
        Par2 { key, parts, complete } => Reply::Par2(cache::par2(tx,key,parts,complete,BUDGET)?),
        RecoveryVolumes(key) => Reply::RecoveryVolumes(cache::recovery_volumes(tx,key,BUDGET)?),
        PutProbe(record) => { cache::put_probe(tx,record,BUDGET)?; Reply::Stored },
        PutNfoPassword { key, password } => { cache::put_nfo_password(tx,key,password,BUDGET)?; Reply::Stored },
        PutStills { key, stills } => { cache::put_stills(tx,key,stills,BUDGET)?; Reply::Stored },
        PutAudioPrints { key, prints } => { cache::put_audio_prints(tx,key,prints,BUDGET)?; Reply::Stored },
        PutArchive(record) => { cache::put_archive(tx,record,BUDGET)?; Reply::Stored },
        PutPar2Files { key, inventory } => { cache::put_par2_files(tx,key,inventory,BUDGET)?; Reply::Stored },
        PutPar2(record) => { cache::put_par2(tx,record,BUDGET)?; Reply::Stored },
        PutRecoveryVolumes { key, volumes } => { cache::put_recovery_volumes(tx,key,volumes,BUDGET)?; Reply::Stored },
    })
}
