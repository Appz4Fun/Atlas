//! The local article store as its files: `articles.db` and the `articles.s{n}.db` shards beside it, each a `Shard`
//! with a write connection of its own, so saves of releases in different files no longer queue for one connection.
//!
//! Every row of a release is in one file (`Store::on`), and a file belongs to one release, so every row of a file is
//! in one file too. `articles.db` keeps the releases it held when the store was first split: `shard_boundary` in its
//! `meta` is the newest release it had then, and every release up to it stays there, later articles included, so a
//! store of any size is split without moving a row. A release above it goes to shard `release_id % shards`. A store
//! made new is split from the start (its boundary is below every id), and `articles.db` holds only its `meta`.
//!
//! The boundary, the number of shards and a token naming this split are written into `articles.db` before any shard
//! is made, and each shard carries the token: a shard from another split, or an `articles.db` an older build wrote a
//! newer release into (one this split would look for in a shard), is refused.
//!
//! A save may write several releases, so several files: each file it wrote keeps its own `unsettled` and `retracted`
//! rows under the save's PostgreSQL transaction, settling asks PostgreSQL once about each transaction waiting in any
//! file and settles every file that waits on it, and `confirm_later` marks it in each file the save wrote.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;

use super::{Ended, FILE, NewParts, NewRow, Shard};

/// The shards a store is split into when first opened by a build that splits it. A store keeps the number it was
/// split into (`meta` key `shards`), so changing this changes only stores made or split afterwards.
const SHARDS: i64 = 8;

/// Transactions `written` remembers at most: one forgotten is only asked about later, as one of another process is.
const WRITTEN_KEPT: usize = 4096;

/// Commits a file keeps waiting for its next `add` at most (`Shard::confirmed`): past it they are marked at once, so
/// a file that sees no more saves doesnt keep them all.
const CONFIRMED_KEPT: usize = 1024;

/// Files `homes` remembers at most before it starts over.
const HOMES_KEPT: usize = 1 << 20;

/// The name of shard `n`'s file.
pub(super) fn shard_file(n: i64) -> String {
    format!("articles.s{n}.db")
}

pub(crate) struct Store {
    /// `articles.db` first, then shard 0, 1, ...: `on` picks one for a release.
    shards: Vec<Shard>,
    /// The newest release `articles.db` held when the store was split: it and every older one stay there.
    boundary: i64,
    writer: bool,
    /// PostgreSQL transactions of this process whose save is still writing (`begin`/`end`), with the groups whose
    /// lock each took (`locked`): settling doesnt ask about them, as they are running, unless it holds the lock of a
    /// group one took (then that one ended, though perhaps on the server's side before this process heard). With helper
    /// writers saving side by side every save found the others' rows waiting and asked PostgreSQL about each, holding
    /// the store meanwhile.
    running: Mutex<HashMap<String, HashSet<String>>>,
    /// The files each transaction of this process wrote or retracted in, for `confirm_later` to mark it in each.
    written: Mutex<HashMap<String, HashSet<usize>>>,
    /// The file each file id's rows were found in (`home`): a file never moves, as its release never does.
    homes: Mutex<HashMap<i64, usize>>,
}

impl Store {
    /// Open the store the indexer writes in `dir`, splitting an unsplit one and making missing shards. `catalogue`
    /// names the PostgreSQL catalogue whose ids the rows carry; a store filled for another one is refused.
    pub(crate) fn open_writer(dir: &Path, catalogue: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let home = Shard::open_writer(&dir.join(FILE), catalogue, super::HOME_CACHE_KIB)?;
        let (boundary, count, token) = match split_of(&home)? {
            Some(split) => {
                // an older build that doesnt split wrote here meanwhile: its newer releases would be looked for in shards
                let newest: Option<i64> = home.lock()?.query_row("select max(release_id) from segments", [], |row| row.get(0))?;
                ensure!(
                    newest.is_none_or(|newest| newest <= split.0),
                    "the local article store {} holds release {} above its split at {}: a build that doesnt split it wrote there; start one that does on a copy from before",
                    home.path().display(),
                    newest.unwrap_or_default(),
                    split.0
                );
                split
            }
            None => {
                let newest: Option<i64> = home.lock()?.query_row("select max(release_id) from segments", [], |row| row.get(0))?;
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness for the local article store's split: {e}"))?;
                let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                let split = (newest.unwrap_or(i64::MIN), SHARDS, token);
                // written before any shard is made, in one transaction: a shard never exists without the split it is of
                let mut conn = home.lock()?;
                let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                for (key, value) in [("shard_boundary", split.0.to_string()), ("shards", split.1.to_string()), ("shard_token", split.2.clone())] {
                    tx.execute("insert into meta (key, value) values (?1, ?2)", params![key, value])?;
                }
                tx.commit()?;
                split
            }
        };
        let mut shards = vec![home];
        for n in 0..count {
            let shard = Shard::open_writer(&dir.join(shard_file(n)), catalogue, super::SHARD_CACHE_KIB)?;
            match shard.meta("shard_token")? {
                Some(theirs) => ensure!(theirs == token, "{} belongs to another split of the local article store", shard.path().display()),
                None => {
                    let empty: bool = shard.lock()?.query_row("select not exists(select 1 from segments)", [], |row| row.get(0))?;
                    ensure!(empty, "{} holds articles but names no split of the local article store", shard.path().display());
                    shard.set_meta("shard_token", &token)?;
                }
            }
            shards.push(shard);
        }
        Ok(Self::of_shards(shards, boundary, true))
    }

    /// Open the store for reading only, as the web process does. Its files must exist and be split: until the
    /// indexer has opened it with a build that splits it, opening fails (and is tried again on the next request).
    pub(crate) fn open_reader(dir: &Path) -> Result<Self> {
        let home = Shard::open_reader(&dir.join(FILE))?;
        let Some((boundary, count, token)) = split_of(&home)? else {
            bail!("the local article store {} isnt split yet: the indexer splits it when it starts", home.path().display());
        };
        let mut shards = vec![home];
        for n in 0..count {
            let shard = Shard::open_reader(&dir.join(shard_file(n)))?;
            ensure!(
                shard.meta("shard_token")?.as_deref() == Some(token.as_str()),
                "{} belongs to another split of the local article store",
                shard.path().display()
            );
            shards.push(shard);
        }
        Ok(Self::of_shards(shards, boundary, false))
    }

    fn of_shards(mut shards: Vec<Shard>, boundary: i64, writer: bool) -> Self {
        let idle = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for shard in &mut shards {
            shard.share_idle_readers(&idle);
        }
        Self {
            shards,
            boundary,
            writer,
            running: Mutex::new(HashMap::new()),
            written: Mutex::new(HashMap::new()),
            homes: Mutex::new(HashMap::new()),
        }
    }

    /// Where `release`'s rows are: `articles.db` up to the boundary, a shard by id above it.
    pub(super) fn index_of(&self, release: i64) -> usize {
        if release <= self.boundary {
            0
        } else {
            1 + release.rem_euclid(self.shards.len() as i64 - 1) as usize
        }
    }

    /// The file holding `release`'s rows.
    pub(super) fn on(&self, release: i64) -> &Shard {
        &self.shards[self.index_of(release)]
    }

    /// The file holding the rows of file `file`, asked of each file in turn (newest shards first) when the reader
    /// doesnt know the file's release. A file without rows anywhere gets `articles.db`, where it has none either, so
    /// whatever is asked of it answers as for a file without articles.
    pub(super) fn home(&self, file: i64) -> Result<&Shard> {
        if let Some(&at) = self.homes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&file) {
            return Ok(&self.shards[at]);
        }
        for at in (1..self.shards.len()).chain([0]) {
            let found = self.shards[at].read(|conn| {
                Ok(conn.prepare_cached("select 1 from segments where file_id = ?1 limit 1")?.exists([file])?)
            })?;
            if found {
                let mut homes = self.homes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if homes.len() >= HOMES_KEPT {
                    homes.clear();
                }
                homes.insert(file, at);
                return Ok(&self.shards[at]);
            }
        }
        Ok(&self.shards[0])
    }

    /// Every file of the store.
    pub(super) fn shards(&self) -> &[Shard] {
        &self.shards
    }

    /// `articles.db`, which also holds the store's own `meta`.
    fn home_shard(&self) -> &Shard {
        &self.shards[0]
    }

    pub(crate) fn path(&self) -> &Path {
        self.home_shard().path()
    }

    pub(crate) fn catalogue(&self) -> Result<Option<String>> {
        self.home_shard().catalogue()
    }

    pub(crate) fn is_writer(&self) -> bool {
        self.writer
    }

    pub(crate) fn meta(&self, key: &str) -> Result<Option<String>> {
        self.home_shard().meta(key)
    }

    pub(crate) fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.home_shard().set_meta(key, value)
    }

    /// Everything committed so far in every file, on disk (`Shard::sync`).
    pub(super) fn sync(&self) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        self.shards.iter().try_for_each(Shard::sync)
    }

    /// For each `(release_id, message_id)`, whether the release already has that article. The keys of each file are
    /// asked of it side by side with the others'.
    pub(crate) fn has(&self, keys: &[(i64, &[u8])]) -> Result<Vec<bool>> {
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); self.shards.len()];
        for (i, (release, _)) in keys.iter().enumerate() {
            groups[self.index_of(*release)].push(i);
        }
        let ask = |at: usize, group: &[usize]| -> Result<Vec<bool>> {
            let asked: Vec<(i64, &[u8])> = group.iter().map(|&i| keys[i]).collect();
            self.shards[at].has(&asked)
        };
        let mut answers = vec![false; keys.len()];
        let busy: Vec<usize> = (0..groups.len()).filter(|&at| !groups[at].is_empty()).collect();
        if busy.len() <= 1 || !self.writer {
            for at in busy {
                for (&i, known) in groups[at].iter().zip(ask(at, &groups[at])?) {
                    answers[i] = known;
                }
            }
            return Ok(answers);
        }
        std::thread::scope(|scope| {
            let asks: Vec<_> = busy.iter().map(|&at| (at, scope.spawn({
                let groups = &groups;
                let ask = &ask;
                move || ask(at, &groups[at])
            }))).collect();
            for (at, ask) in asks {
                let found = ask.join().map_err(|_| anyhow::anyhow!("a local store lookup panicked"))??;
                for (&i, known) in groups[at].iter().zip(found) {
                    answers[i] = known;
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        Ok(answers)
    }

    /// Write the accepted articles of one batch and remember them under `xid`,
    /// the PostgreSQL transaction that counts them. Returns the part numbers
    /// each file gained, `0..=max_part` only, as `file_parts` used to report.
    /// A row the release already has is an error: the caller asked `has` first.
    pub(crate) fn add(&self, xid: &str, rows: &[NewRow<'_>], max_part: i64) -> Result<Vec<NewParts>> {
        self.add_fresh(xid, rows, max_part, &HashSet::new())
    }

    /// `add`, where the files in `fresh` were made in the same PostgreSQL transaction and have no articles here yet:
    /// their parts arent looked up. Each file the rows go to is written under its own lock, one after another, so a
    /// save holds one file's write connection at a time; saves of different files write side by side. Should one
    /// fail after another committed, the caller's PostgreSQL transaction fails and settling takes those rows out.
    pub(crate) fn add_fresh(&self, xid: &str, rows: &[NewRow<'_>], max_part: i64, fresh: &HashSet<i64>) -> Result<Vec<NewParts>> {
        ensure!(self.writer, "the local article store is open for reading only");
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); self.shards.len()];
        for (i, row) in rows.iter().enumerate() {
            groups[self.index_of(row.release_id)].push(i);
        }
        let busy: Vec<usize> = (0..groups.len()).filter(|&at| !groups[at].is_empty()).collect();
        self.wrote(xid, &busy);
        // each file's gains with the place in `rows` of the row that first counted, so they come back in that order
        let mut gained = Vec::new();
        for at in busy {
            let group: Vec<&NewRow<'_>> = groups[at].iter().map(|&i| &rows[i]).collect();
            for (local, parts) in self.shards[at].add(xid, &group, max_part, fresh)? {
                gained.push((groups[at][local], parts));
            }
        }
        gained.sort_unstable_by_key(|&(at, _)| at);
        Ok(gained.into_iter().map(|(_, parts)| parts).collect())
    }

    /// `xid` wrote in the files `shards`.
    fn wrote(&self, xid: &str, shards: &[usize]) {
        let mut written = self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if written.len() >= WRITTEN_KEPT && !written.contains_key(xid) {
            written.clear();
        }
        written.entry(xid.to_owned()).or_default().extend(shards);
    }

    /// The caller saw PostgreSQL commit `xid`: its rows stay and stop waiting, in every file.
    pub(crate) fn confirm(&self, xid: &str) -> Result<()> {
        self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(xid);
        self.shards.iter().try_for_each(|shard| shard.confirm(xid))
    }

    /// The save under `xid` deleted these releases from the catalogue. Their
    /// articles are dropped when it commits and kept if it rolls back.
    pub(crate) fn retract(&self, xid: &str, releases: &[i64]) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        let mut groups: Vec<Vec<i64>> = vec![Vec::new(); self.shards.len()];
        for &release in releases {
            groups[self.index_of(release)].push(release);
        }
        let busy: Vec<usize> = (0..groups.len()).filter(|&at| !groups[at].is_empty()).collect();
        self.wrote(xid, &busy);
        for at in busy {
            self.shards[at].retract(xid, &groups[at])?;
        }
        Ok(())
    }

    /// Whether any save is still waiting to hear how its transaction ended.
    pub(crate) fn has_unsettled(&self) -> Result<bool> {
        self.has_unsettled_besides(None, None)
    }

    /// Transaction `xid` of this process is saving, having taken the locks of `groups` so far: until `end`, settling
    /// takes it for running without asking, except under one of those groups' locks. Call only while its transaction
    /// is open as far as this process knows (before its commit is sent or its rollback), and `end` before either.
    pub(crate) fn begin(&self, xid: &str, groups: &[String]) {
        let mut running = self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        running.entry(xid.to_owned()).or_default().extend(groups.iter().cloned());
    }

    /// Transaction `xid`, saving, took `group`'s lock.
    pub(crate) fn locked(&self, xid: &str, group: &str) {
        let mut running = self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(groups) = running.get_mut(xid) {
            groups.insert(group.to_owned());
        }
    }

    pub(crate) fn end(&self, xid: &str) {
        self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(xid);
    }

    /// Whether settling may take `xid` for running without asking: it is saving in this process, and didnt take the
    /// lock of `under`, the group whose lock the settling transaction holds (an `xid` that did has ended since).
    /// (One seen to commit is left out by `Shard::unsettled` already.)
    fn skips(&self, xid: &str, under: Option<&str>) -> bool {
        let running = self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        running.get(xid).is_some_and(|groups| under.is_none_or(|group| !groups.contains(group)))
    }

    /// The caller saw PostgreSQL commit `xid`: as `confirm`, marked in the next `add`'s transaction of each file it
    /// wrote in instead of one of its own. Lost if the process ends first, then asked about as any unsettled
    /// transaction is.
    pub(crate) fn confirm_later(&self, xid: &str) {
        let Some(shards) = self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(xid) else { return };
        for at in shards {
            let shard = &self.shards[at];
            let full = {
                let mut confirmed = shard.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                confirmed.insert(xid.to_owned());
                confirmed.len() > CONFIRMED_KEPT
            };
            // a file no save writes any more: its waiting commits are marked now, each in a transaction of its own
            if full {
                let waiting: Vec<String> = shard.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().cloned().collect();
                for xid in waiting {
                    if shard.confirm(&xid).is_ok() {
                        shard.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&xid);
                    }
                }
            }
        }
    }

    /// For each file, the transactions with rows waiting in it, but those `skips` lets be.
    fn waiting(&self, under: Option<&str>) -> Result<Vec<Vec<String>>> {
        self.shards
            .iter()
            .map(|shard| Ok(shard.unsettled()?.into_iter().filter(|xid| !self.skips(xid, under)).collect()))
            .collect()
    }

    /// Ask `ended` about every transaction with rows still waiting. Rows of a
    /// committed one stay; rows of a rolled-back one are taken out again.
    pub(crate) fn settle(&self, ended: impl FnMut(&str) -> Result<Ended>) -> Result<()> {
        self.settle_under(None, ended)
    }

    /// `settle`, by a transaction holding the lock of group `under`: a save of this process that took it is asked
    /// about too, though it hasnt said it ended. Each transaction is asked about once, however many files it wrote in.
    pub(crate) fn settle_under(&self, under: Option<&str>, mut ended: impl FnMut(&str) -> Result<Ended>) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        let waiting = self.waiting(under)?;
        // PostgreSQL is asked with the store unlocked, so readers are not kept waiting on it.
        let mut answers = HashMap::new();
        for xid in waiting.iter().flatten() {
            if !answers.contains_key(xid) {
                answers.insert(xid.clone(), ended(xid)?);
            }
        }
        // a file nothing ended in: its write connection, which every save of it waits for, isnt taken
        for (shard, xids) in self.shards.iter().zip(&waiting) {
            let ended: Vec<(String, Ended)> =
                xids.iter().map(|xid| (xid.clone(), answers[xid])).filter(|(_, how)| *how != Ended::Running).collect();
            shard.settle(&ended)?;
        }
        Ok(())
    }

    /// Whether a save other than `own` is still waiting to hear how its transaction ended.
    /// (`under`: the group whose lock the asking transaction holds, see `settle_under`.)
    pub(crate) fn has_unsettled_besides(&self, own: Option<&str>, under: Option<&str>) -> Result<bool> {
        Ok(self.waiting(under)?.iter().flatten().any(|xid| Some(xid.as_str()) != own))
    }

    /// Bulk load rows that PostgreSQL already counted, as the one-time move of
    /// an existing catalogue does. Rows the store already has are skipped.
    pub(crate) fn import(&self, rows: &[NewRow<'_>]) -> Result<u64> {
        ensure!(self.writer, "the local article store is open for reading only");
        let mut groups: Vec<Vec<&NewRow<'_>>> = vec![Vec::new(); self.shards.len()];
        for row in rows {
            groups[self.index_of(row.release_id)].push(row);
        }
        let mut added = 0;
        for (shard, group) in self.shards.iter().zip(&groups) {
            if !group.is_empty() {
                added += shard.import(group)?;
            }
        }
        Ok(added)
    }

    /// A fixture's `delete from segments where release_id = ..`.
    #[cfg(test)]
    pub(crate) fn forget_releases(&self, releases: &[i64]) -> Result<u64> {
        let mut gone = 0;
        for &release in releases {
            gone += self.on(release).forget_releases(&[release])?;
        }
        Ok(gone)
    }
}

/// The split `articles.db` records: boundary, number of shards and token. `None` for a store no build split yet.
fn split_of(home: &Shard) -> Result<Option<(i64, i64, String)>> {
    let (Some(boundary), Some(count), Some(token)) = (home.meta("shard_boundary")?, home.meta("shards")?, home.meta("shard_token")?) else {
        return Ok(None);
    };
    let count: i64 = count.parse().context("the local article store's number of shards is not a number")?;
    ensure!(count >= 1, "the local article store names {count} shards");
    Ok(Some((boundary.parse().context("the local article store's split is not a number")?, count, token)))
}
