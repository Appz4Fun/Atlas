//! Whether a release is one upload, or the same post uploaded again and again under the same subjects (twins
//! merged into one row), and which upload each article is. Only posting time tells uploads apart: their articles
//! are the same bytes under other message-ids. A part number seen twice in a file is not enough on its own: files
//! that share a row (a subject with no file name, or the same one for each) repeat part numbers too. The release is
//! split only when every test of `decide` passes; otherwise it stays one release.

use std::collections::{HashMap, HashSet};

/// Two uploads are at least this far apart in time, in seconds.
pub const MIN_GAP_SECS: i64 = 180;
/// ... and at least this many times the release's median gap between consecutive articles.
pub const MIN_GAP_TIMES_MEDIAN: i64 = 20;
/// Each upload holds at least this share of the release's distinct (file, part) pairs, per mille.
pub const MIN_COVER_PER_MILLE: usize = 900;
/// The copies of a part differ in size by at most this, per mille of the largest, or `MAX_BYTES_SPREAD`, whichever is
/// more: headers differ, bodies dont. Measured on five uploads of one post: up to 5,766 bytes on 740 KB articles
/// (0.78%), 273 bytes on a 5 KB last article.
pub const MAX_BYTES_SPREAD_PER_MILLE: i64 = 10;
pub const MAX_BYTES_SPREAD: i64 = 8_192;

/// One stored article: its file and part, the part total its subject gave, its size and its Date (unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    pub file: String,
    pub part: Option<i64>,
    pub total: Option<i64>,
    pub bytes: i64,
    pub date: Option<i64>,
}

/// Why a release stays one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// no part under two message-ids
    NoCopies,
    /// an article without a Date or a part number
    Unknown,
    /// the articles dont fall into two or more windows
    OneWindow,
    /// some part has two copies in one window, or none of a window's copies is the only one in it
    Interleaved,
    /// a window holds too little of the post to be an upload of it
    Partial,
    /// copies of one part differ in their total or their size
    Different,
}

/// What `decide` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep(Keep),
    /// one upload per window, in time order: `uploads[i]` = the upload of `copies[i]`, counted from 0
    Split {
        windows: usize,
        uploads: Vec<usize>,
    },
}

fn median(mut v: Vec<i64>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// Whether `copies` (every article a release holds) are several whole uploads of the same post, and which is which.
pub fn decide(copies: &[Copy]) -> Verdict {
    let Some(dated): Option<Vec<(i64, &str, i64)>> =
        copies.iter().map(|c| Some((c.date?, c.file.as_str(), c.part?))).collect()
    else {
        return Verdict::Keep(Keep::Unknown);
    };
    let mut per_part: HashMap<(&str, i64), Vec<usize>> = HashMap::new();
    for (i, (_, file, part)) in dated.iter().enumerate() {
        per_part.entry((*file, *part)).or_default().push(i);
    }
    if per_part.values().all(|v| v.len() < 2) {
        return Verdict::Keep(Keep::NoCopies);
    }

    // windows: the articles in time order, cut where the gap is far wider than any gap inside an upload
    let mut order: Vec<usize> = (0..dated.len()).collect();
    order.sort_by_key(|i| dated.get(*i).map(|d| d.0));
    let gaps: Vec<i64> = order.windows(2).filter_map(|w| Some(dated.get(w[1])?.0 - dated.get(w[0])?.0)).collect();
    let cut = MIN_GAP_SECS.max(MIN_GAP_TIMES_MEDIAN.saturating_mul(median(gaps.clone())));
    let mut uploads = vec![0usize; dated.len()];
    let mut window = 0usize;
    for (k, i) in order.iter().enumerate() {
        if k > 0 && gaps.get(k - 1).is_some_and(|g| *g >= cut) {
            window += 1;
        }
        if let Some(u) = uploads.get_mut(*i) {
            *u = window;
        }
    }
    let windows = window + 1;
    if windows < 2 {
        return Verdict::Keep(Keep::OneWindow);
    }

    // every copy of a part in a window of its own
    for idx in per_part.values() {
        let mut seen = HashSet::new();
        if !idx.iter().all(|i| uploads.get(*i).is_some_and(|w| seen.insert(*w))) {
            return Verdict::Keep(Keep::Interleaved);
        }
    }
    // every window an upload of (nearly) the whole post
    let pairs = per_part.len();
    let mut cover = vec![0usize; windows];
    for w in &uploads {
        if let Some(c) = cover.get_mut(*w) {
            *c += 1;
        }
    }
    if cover.iter().any(|c| c * 1000 < pairs * MIN_COVER_PER_MILLE) {
        return Verdict::Keep(Keep::Partial);
    }
    // the copies of a part: the same total, the same size give or take their headers
    for idx in per_part.values().filter(|v| v.len() > 1) {
        let parts: Vec<&Copy> = idx.iter().filter_map(|i| copies.get(*i)).collect();
        let totals: HashSet<Option<i64>> = parts.iter().map(|c| c.total).collect();
        let (lo, hi) = parts.iter().fold((i64::MAX, i64::MIN), |(lo, hi), c| (lo.min(c.bytes), hi.max(c.bytes)));
        let slack = MAX_BYTES_SPREAD.max(hi.saturating_mul(MAX_BYTES_SPREAD_PER_MILLE) / 1000);
        if totals.len() > 1 || hi - lo > slack {
            return Verdict::Keep(Keep::Different);
        }
    }
    Verdict::Split { windows, uploads }
}

/// Overview lines read at once, while looking for a release's articles.
const DATE_CHUNK: u64 = 20_000;
/// How much earlier and later than the release's first and last Date the search for its articles starts and ends:
/// post dates are only roughly in order.
const DATE_MARGIN_SECS: i64 = 600;

/// The Date of each of `ids` (message-ids, with or without their angle brackets) on server `i`, read from its
/// overview of `group` between `from` and `to` (unix seconds, the release's first and last Date): `article_at` finds
/// the article numbers there, then the range is read `DATE_CHUNK` at a time. Article numbers are the server's own, soo
/// one server is asked for both. None when the range holds more than `max_numbers` articles. An id not found is left
/// out (the server has expired it, or numbered it outside the range).
pub async fn dates_on(
    pool: &crate::nntp::Pool,
    i: usize,
    group: &str,
    (from, to): (i64, i64),
    ids: &HashSet<String>,
    max_numbers: u64,
) -> crate::nntp::Result<Option<HashMap<String, i64>>> {
    let bare = |id: &str| id.trim().trim_start_matches('<').trim_end_matches('>').to_string();
    let wanted: HashSet<String> = ids.iter().map(|id| bare(id)).collect();
    let (_, low, high, _) = pool.group_on(i, group).await?;
    let start = pool.article_at(i, group, low, high, from.saturating_sub(DATE_MARGIN_SECS)).await?;
    let end = pool.article_at(i, group, low, high, to.saturating_add(DATE_MARGIN_SECS)).await?.min(high);
    if end < start {
        return Ok(Some(HashMap::new()));
    }
    if end - start + 1 > max_numbers {
        return Ok(None);
    }
    let mut out = HashMap::new();
    let mut at = start;
    while at <= end && out.len() < wanted.len() {
        let last = at.saturating_add(DATE_CHUNK - 1).min(end);
        let rows = match pool.xover_on(i, group, at, last).await {
            Err(e) if e.is_empty_range() => Vec::new(),
            r => r?,
        };
        for o in rows {
            let id = bare(&o.message_id);
            if wanted.contains(&id)
                && let Some(t) = crate::nntp::plausible_post_time(&o.date)
            {
                out.insert(id, t);
            }
        }
        at = last.saturating_add(1);
    }
    Ok(Some(out))
}

/// Whether a row could be several uploads, without the network: its files hold `k` articles for each part they
/// have seen, `k` between 2 and 20 (within 2%), over at least 50 parts. Rows whose files share part numbers for other
/// reasons pass too; `decide` tells them apart once it has the Dates.
pub fn looks_reposted(parts: i64, seen: i64) -> bool {
    if seen < 50 {
        return false;
    }
    let k = (parts as f64 / seen as f64).round();
    (2.0..=20.0).contains(&k) && (parts as f64 - k * seen as f64).abs() <= 0.02 * k * seen as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(file: &str, part: i64, total: i64, bytes: i64, date: i64) -> Copy {
        Copy { file: file.into(), part: Some(part), total: Some(total), bytes, date: Some(date) }
    }

    /// `uploads` uploads of a post of `files` files of `parts` parts, each upload starting at its own second and
    /// posting an article every second, every copy of a part a few bytes apart.
    fn post(files: usize, parts: i64, starts: &[i64]) -> Vec<Copy> {
        let mut out = Vec::new();
        for (u, start) in starts.iter().enumerate() {
            let mut t = *start;
            for f in 0..files {
                for p in 1..=parts {
                    out.push(copy(&format!("f{f}.rar"), p, parts, 730_000 + u as i64 * 7, t));
                    t += 1;
                }
            }
        }
        out
    }

    /// The Flight of Dragons: five uploads of about 7 minutes, 5 to 33 minutes apart.
    #[test]
    fn five_uploads_in_their_own_windows_are_five_twins() {
        let starts = [0, 40 * 60, 56 * 60, 71 * 60, 84 * 60];
        let copies = post(4, 100, &starts);
        let Verdict::Split { windows, uploads } = decide(&copies) else { panic!("{:?}", decide(&copies)) };
        assert_eq!(windows, 5);
        for (i, u) in uploads.iter().enumerate() {
            assert_eq!(*u, i / 400, "article {i} is in its own upload");
        }
    }

    /// Headers differ between uploads: 5,766 bytes on a 740 KB article, 273 on a 5 KB last one (measured).
    #[test]
    fn header_noise_on_small_and_large_articles_is_the_same_part() {
        let mut copies = post(1, 50, &[0, 3_600]);
        for c in copies.iter_mut() {
            let second = c.date.is_some_and(|d| d >= 3_600);
            match (c.part, second) {
                (Some(50), false) => c.bytes = 5_246,
                (Some(50), true) => c.bytes = 5_246 - 273,
                (_, true) => c.bytes = 740_000 - 5_766,
                _ => c.bytes = 740_000,
            }
        }
        assert!(matches!(decide(&copies), Verdict::Split { windows: 2, .. }), "{:?}", decide(&copies));
    }

    /// Topaz: two uploads six days apart.
    #[test]
    fn two_uploads_days_apart_are_two_twins() {
        let copies = post(2, 164, &[0, 6 * 86_400]);
        assert!(matches!(decide(&copies), Verdict::Split { windows: 2, .. }));
    }

    #[test]
    fn the_rows_worth_reading_dates_for() {
        assert!(looks_reposted(273_344, 54_669), "five uploads, one article missing");
        assert!(looks_reposted(328, 164));
        assert!(looks_reposted(199, 100), "two uploads, one article missing");
        assert!(!looks_reposted(26, 1), "too few parts to tell");
        assert!(!looks_reposted(5_000, 3_000), "not a whole number of copies");
        assert!(!looks_reposted(54_669, 54_669), "one upload");
        assert!(!looks_reposted(30_000, 1_000), "more copies than any repost");
    }

    #[test]
    fn one_upload_stays_one() {
        assert_eq!(decide(&post(3, 50, &[0])), Verdict::Keep(Keep::NoCopies));
    }

    /// Images posted one after another under one name, each `(1/1)`: the same part number again and again, but no
    /// windows of whole uploads and sizes that differ.
    #[test]
    fn different_files_under_one_name_stay_one() {
        let copies: Vec<Copy> = (0..26).map(|i| copy("000-108.JPG", 1, 1, 150_000 + i * 9_000, i * 30)).collect();
        assert!(matches!(decide(&copies), Verdict::Keep(Keep::OneWindow | Keep::Interleaved)), "{:?}", decide(&copies));
        // two of them a day apart: their sizes give them away
        let mut apart = post(1, 1, &[0, 86_400]);
        if let Some(c) = apart.last_mut() {
            c.bytes += 40_000;
        }
        apart.extend((0..20).map(|i| copy(&format!("other{i}.jpg"), 1, 1, 9_000, i)));
        apart.extend((0..20).map(|i| copy(&format!("other{i}.jpg"), 1, 1, 9_000, 86_400 + i)));
        assert_eq!(decide(&apart), Verdict::Keep(Keep::Different));
    }

    /// A few parts posted again later (a fill post for missing articles) isnt an upload of the post.
    #[test]
    fn a_fill_post_stays_part_of_the_release() {
        let mut copies = post(2, 100, &[0]);
        copies.extend((1..=5).map(|p| copy("f0.rar", p, 100, 730_003, 7_200 + p)));
        assert_eq!(decide(&copies), Verdict::Keep(Keep::Partial));
    }

    /// Two uploads posted at the same time (overlapping windows) cant be told apart.
    #[test]
    fn overlapping_uploads_stay_one() {
        let mut copies = post(1, 200, &[0]);
        copies.extend(post(1, 200, &[100]).into_iter().map(|c| Copy { bytes: c.bytes + 3, ..c }));
        assert!(matches!(decide(&copies), Verdict::Keep(Keep::OneWindow | Keep::Interleaved)));
    }

    #[test]
    fn copies_with_another_total_stay_one() {
        let mut copies = post(1, 100, &[0]);
        copies.extend(post(1, 100, &[86_400]).into_iter().map(|c| Copy { total: Some(101), ..c }));
        assert_eq!(decide(&copies), Verdict::Keep(Keep::Different));
    }

    #[test]
    fn an_undated_article_keeps_it_whole() {
        let mut copies = post(1, 10, &[0, 86_400]);
        if let Some(c) = copies.first_mut() {
            c.date = None;
        }
        assert_eq!(decide(&copies), Verdict::Keep(Keep::Unknown));
    }
}
