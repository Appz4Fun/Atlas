//! Shared bounded admission and authenticated overview facts for the native Spotnet lanes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::store;
use super::{header, sign};

/// Overview lines asked for in one XOVER.
pub(super) const XOVER_STEP: u64 = 5_000;
/// The largest article of a spot's NZB read, as it comes off the wire: they are a few hundred KB.
pub(super) const MAX_NZB_ARTICLE: usize = 4 * 1024 * 1024;
/// A spot's NZB articles read at most, and their bytes together. A real spot names one or two; past these the
/// spot is no real one, and nothing more is read.
const MAX_NZB_ARTICLES: usize = 16;
pub(super) const MAX_NZB_POSTED: usize = 16 * 1024 * 1024;
/// What `body_capped` may read past its cap before it gives up: one long line.
pub(super) const CAP_OVERSHOOT: usize = 64 * 1024;
/// Maximum simultaneous decode permits held through native publication.
const DECODING: usize = 4;
/// NZB article bytes one pass reads at most; the spots left wait for the next pass.
pub(super) const MAX_PASS_POSTED: usize = 256 * 1024 * 1024;
/// Spots saved before their XML was kept, read again per pass (a HEAD each, a few KB).
pub(super) const XML_BACKFILL: usize = 50;

/// What one pass did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub read: u64,
    pub noted: usize,
    pub saved: usize,
    pub skipped: usize,
    pub retry: usize,
    /// left for a later pass: the pass's byte budget ran out, or a spot with the same NZB was being saved
    pub pending: usize,
}

/// What a pass's spot tasks share.
pub(super) struct Shared {
    /// NZB article bytes the pass may still read: each BODY reserves the most it can take first, and gives back
    /// what it didn't read
    pub(super) budget: AtomicUsize,
    /// `DECODING` spots at a time from inflating their NZB to tagging their release: decoding is the memory a spot
    /// takes (an NZB of up to 64 MB, up to 300,000 articles), held until its save is done
    pub(super) decoding: tokio::sync::Semaphore,
    /// NZB keys (and NZB article sets, `ref:`) of spots being read, saved or tagged in this pass whose outcome isn't
    /// in spots_seen yet
    claims: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Shared {
    pub(super) fn new(budget: usize) -> Shared {
        Shared {
            budget: AtomicUsize::new(budget),
            decoding: tokio::sync::Semaphore::new(DECODING),
            claims: Default::default(),
        }
    }

    /// Takes `n` bytes of the budget, or nothing when fewer are left.
    pub(super) fn reserve(&self, n: usize) -> bool {
        self.budget.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(n)).is_ok()
    }

    pub(super) fn give_back(&self, n: usize) {
        self.budget.fetch_add(n, Ordering::Relaxed);
    }

    /// Claims an NZB key for this spot until the guard goes, or None when another spot holds it.
    pub(super) fn claim(self: &Arc<Self>, key: &str) -> Option<Claim> {
        let mut claims = self.claims.lock().unwrap();
        claims.insert(key.to_string()).then(|| Claim { shared: self.clone(), key: key.to_string() })
    }
}

/// An NZB key claimed in `Shared::claims`, released when dropped.
pub(super) struct Claim {
    shared: Arc<Shared>,
    key: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.shared.claims.lock().unwrap().remove(&self.key);
    }
}

pub fn start_at(cursor: Option<u64>, low: u64, high: u64, first: u64) -> u64 {
    match cursor {
        Some(c) if c <= high => c + 1,
        _ => high.saturating_sub(first).max(low).max(1),
    }
}

/// The removal an overview line is, when it's a moderator's (or a spotter's own) `DISPOSE` whose signatures hold.
/// A spotter's own needs their key to have signed its title and message-id: it removes only their posts.
pub(super) fn removal(r: &crate::nntp::Overview, now: i64) -> Option<store::Disposal> {
    let h = header::parse(&r.subject, &r.from)?;
    let message_id = h.removes()?;
    let verdict = sign::verify(&h.title, &r.from, &r.message_id);
    if !verdict.verified {
        return None;
    }
    // their key must sign both the title, which names the target, and the message-id: the header signature alone
    // could be replayed onto another message-id, the message-id's alone onto another title
    let spotter = match verdict.personal {
        true if verdict.message_id_signed && verdict.title_signed => verdict.spotter_id?,
        true => return None,
        false => String::new(),
    };
    let at = crate::dates::posted_timestamp(&r.date).unwrap_or(now);
    Some(store::Disposal { message_id, spotter, at })
}

pub(super) const SETTLE_BATCH: usize = 200;

pub(super) fn nzb_ids_usable(ids: &[String]) -> bool {
    let mut seen = std::collections::HashSet::new();
    ids.len() <= MAX_NZB_ARTICLES && ids.iter().all(|id| seen.insert(id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_pass_starts() {
        // the first pass goes `first` back, but not before the group's first article
        assert_eq!(start_at(None, 2, 4_450_733, 20_000), 4_430_733);
        assert_eq!(start_at(None, 4_440_000, 4_450_733, 20_000), 4_440_000);
        assert_eq!(start_at(None, 0, 10, 20_000), 1);
        // then after the cursor
        assert_eq!(start_at(Some(4_450_000), 2, 4_450_733, 20_000), 4_450_001);
        assert_eq!(start_at(Some(4_450_733), 2, 4_450_733, 20_000), 4_450_734);
        // a cursor past the newest article: the server renumbered
        assert_eq!(start_at(Some(9_000_000), 2, 4_450_733, 20_000), 4_430_733);
    }

    #[test]
    fn the_pass_budget_is_reserved_before_reading_and_given_back_after() {
        let shared = Shared::new(100);
        assert!(shared.reserve(60));
        assert!(!shared.reserve(60), "only 40 left");
        shared.give_back(50);
        assert!(shared.reserve(90));
        assert_eq!(shared.budget.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_nzb_key_is_claimed_by_one_spot_until_its_guard_goes() {
        let shared = Arc::new(Shared::new(0));
        let first = shared.claim("k").unwrap();
        assert!(shared.claim("k").is_none());
        assert!(shared.claim("other").is_some());
        drop(first);
        assert!(shared.claim("k").is_some());
    }

    #[test]
    fn nzb_ids_are_few_and_named_once() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(nzb_ids_usable(&ids(&["a@spot.net", "b@spot.net"])));
        assert!(!nzb_ids_usable(&ids(&["a@spot.net", "a@spot.net"])));
        let many: Vec<String> = (0..=MAX_NZB_ARTICLES).map(|n| format!("{n}@spot.net")).collect();
        assert!(!nzb_ids_usable(&many));
        assert!(nzb_ids_usable(&many[..MAX_NZB_ARTICLES]));
    }
}
