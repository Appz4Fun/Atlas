//! Pure posting/file decisions shared by legacy reads and bounded native PG
//! snapshots. No backend, database handle, error dialect or write authority.
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap,BTreeSet,HashMap,HashSet};
use crate::store;
use super::{SplitPost,post_stem,LINKED_ROWS};

/// How far apart a split post's groups can have got their articles, in days. Each row keeps its newest article's date,
/// and real ones are minutes apart.
pub(super) const SPLIT_POST_DAYS: f64 = 1.0;

/// The most groups a split post can span. Real ones have a handful, and every file of its NZB lists them all.
const SPLIT_POST_GROUPS: usize = 16;

/// A row named as a post, in some shard.
#[derive(Clone)]
pub(crate) struct Named {
    pub(crate) id: i64,
    pub(crate) group: String,
    pub(crate) bytes: i64,
    pub(crate) date: Option<f64>,
    pub(crate) poster: Option<String>,
}

/// Existing pure exact-name/poster/XPOST decision, shared by bounded native PG reads.
pub(crate) fn exact_named<E>(anchor:i64,same:&[Named],named:&[Named],load:&mut impl FnMut(&[i64])->std::result::Result<UnionFiles,E>)->std::result::Result<Option<SplitPost>,E> {
    let same: Vec<&Named> = same.iter().collect();
    let (across, shared, claimed) = across_posters(load, anchor, named)?;
    // a row in a post sharing parts gets that post: its poster's own rows could otherwise make one beside it
    if shared {
        return Ok(across.map(|(post, _)| post));
    }
    let by_poster = same_poster_post(load, &same)?;
    // one poster can have several of the groups: the post across posters is taken only if it holds all of those
    match (by_poster, across) {
        // a row of the poster's post may be in the other as a crosspost of it
        (Some(one), Some((across, covers)))
            if one.ids.iter().all(|id| covers.contains(id)) && covers.len() > one.ids.len() =>
        {
            Ok(Some(across))
        }
        // a poster's own rows can't be a post where one of them is in a post sharing parts: every row gets that one
        (Some(one), across) if one.ids.iter().any(|id| claimed.contains(id)) => Ok(across.map(|(post, _)| post)),
        (Some(one), _) => Ok(Some(one)),
        (None, across) => Ok(across.map(|(post, _)| post)),
    }
}

fn same_poster_post<E>(load: &mut impl FnMut(&[i64])->std::result::Result<UnionFiles,E>, rows: &[&Named]) -> std::result::Result<Option<SplitPost>,E> {
    let dates: Vec<Option<f64>> = rows.iter().map(|n| n.date).collect();
    if !(2..=SPLIT_POST_GROUPS).contains(&rows.len()) || !close_together(&dates) {
        return Ok(None);
    }
    let mut ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
    let mut groups: Vec<String> = rows.iter().map(|n| n.group.clone()).collect();
    let size = rows.iter().map(|n| n.bytes).sum();
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    let files = load( &ids)?;
    // rows sharing parts join only as `across_posters` grows a post, row by row, each adding what the rest lack
    if !files.one_upload() || files.overlap {
        return Ok(None);
    }
    Ok(Some(SplitPost {
        ids,
        groups,
        size,
        complete: files.complete(),
        lead: None,
        recovery: Vec::new(),
        beside: Vec::new(),
    }))
}

/// The most rows of one name looked at for a post whose groups each got a poster of their own.
const ACROSS_POSTERS_ROWS: usize = 32;

/// `across_posters`'s answer: the post the row asking is in, with the rows it stands for; whether that post's rows
/// share parts; and every row in a post of two rows or more that shares parts, whichever.
type AcrossPosters = (Option<(SplitPost, Vec<i64>)>, bool, HashSet<i64>);

/// One post whose groups each got another made-up poster (Nyuu and ngPost can give every connection its own From): the
/// rows share the post's files, and each holds parts of them no other row has. A name and a subject are only headers,
/// soo rows join only where they fill each other's gaps, file by file, under the same subject, and a row that is a
/// crosspost of another (the same parts of the same files) is left out of the post, its group kept. Rows are taken
/// lowest id first whichever row asks, soo every row of the post gets the same answer.
fn across_posters<E>(load: &mut impl FnMut(&[i64])->std::result::Result<UnionFiles,E>, anchor: i64, named: &[Named]) -> std::result::Result<AcrossPosters,E> {
    // every row of the name, whichever asks: a dated one, lowest ids first
    let mut rows: Vec<&Named> = named.iter().filter(|n| n.date.is_some()).collect();
    if rows.len() < 2 || !rows.iter().any(|n| n.id == anchor) {
        return Ok((None, false, HashSet::new()));
    }
    let one_poster = rows.iter().all(|n| n.poster == rows[0].poster);
    // one poster's rows are `same_poster_post`'s unless some share parts, which one read of them all tells, where
    // they could be one post at all
    if one_poster {
        let dates: Vec<Option<f64>> = rows.iter().map(|n| n.date).collect();
        if rows.len() > SPLIT_POST_GROUPS || !close_together(&dates) {
            return Ok((None, false, HashSet::new()));
        }
        let ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
        if !load( &ids)?.overlap {
            return Ok((None, false, HashSet::new()));
        }
    }
    rows.sort_by_key(|n| n.id);
    // each poster's own rows, as `same_poster_post` reads them
    let mut by_poster: HashMap<&str, Vec<&Named>> = HashMap::new();
    for n in &rows {
        if let Some(poster) = n.poster.as_deref() {
            by_poster.entry(poster).or_default().push(*n);
        }
    }
    rows.truncate(ACROSS_POSTERS_ROWS);
    // each row alone, to tell a crosspost from a part
    let mut kept: Vec<(&Named, UnionFiles)> = Vec::new();
    let mut copies: Vec<(&Named, i64)> = Vec::new();
    for n in rows {
        let files = load( &[n.id])?;
        if files.files.is_empty() {
            continue;
        }
        // a crosspost is posted with what it copies: an older upload of the same parts is a post of its own
        let near = |of: &Named| matches!((of.date, n.date), (Some(a), Some(b)) if (a - b).abs() <= SPLIT_POST_DAYS);
        match kept.iter().find(|(of, k)| near(of) && k.same_parts(&files)) {
            Some((of, _)) => copies.push((n, of.id)),
            None => kept.push((n, files)),
        }
    }
    // the rows fall into posts, each grown from its lowest row left, of rows sharing no part: the same posts whoever
    // asks, and as before rows could share parts
    let mut posts: Vec<(Vec<i64>, UnionFiles)> = Vec::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for seed in 0..kept.len() {
        if taken.contains(&kept[seed].0.id) {
            continue;
        }
        let (chosen, union) = grow_post(load, &kept, seed, &taken, false)?;
        taken.extend(chosen.iter().copied());
        posts.push((chosen, union));
    }
    // then, where rows in no whole post share parts (a file uploaded again within the hour, each upload spread over
    // the groups), those rows fall into posts again, rows sharing parts allowed (`fills_gaps`)
    let whole = |chosen: &[i64], union: &UnionFiles| chosen.len() > 1 && union.one_upload() && union.complete();
    let in_whole: HashSet<i64> =
        posts.iter().filter(|(c, u)| whole(c, u)).flat_map(|(c, _)| c.iter().copied()).collect();
    let mut rest: Vec<usize> = (0..kept.len()).filter(|i| !in_whole.contains(&kept[*i].0.id)).collect();
    let shares = |rest: &[usize]| {
        rest.iter().enumerate().any(|(n, a)| rest[n + 1..].iter().any(|b| kept[*a].1.shares_parts(&kept[*b].1)))
    };
    let mut sharing = shares(&rest);
    let mut in_whole = in_whole;
    if sharing {
        // nor do the rows of a poster's own whole post: they keep it
        for own in by_poster.values() {
            if let Some(post) = same_poster_post(load, own)?
                && post.complete
            {
                in_whole.extend(post.ids);
            }
        }
        rest.retain(|i| !in_whole.contains(&kept[*i].0.id));
        sharing = shares(&rest);
    }
    if sharing {
        // only a whole post sharing parts is kept: the rows of one that isn't keep the answers they had
        let mut taken = in_whole.clone();
        let mut sharing_whole: Vec<(Vec<i64>, UnionFiles)> = Vec::new();
        for seed in &rest {
            if taken.contains(&kept[*seed].0.id) {
                continue;
            }
            let (chosen, union) = grow_post(load, &kept, *seed, &taken, true)?;
            taken.extend(chosen.iter().copied());
            if union.overlap && whole(&chosen, &union) {
                sharing_whole.push((chosen, union));
            }
        }
        if !sharing_whole.is_empty() {
            // the whole posts sharing parts, and every other post as before rows could share parts, unless a row of
            // it joined one: its other rows are left with no post. One poster's other rows had no post across
            // posters before, and have none now
            let joined: HashSet<i64> = sharing_whole.iter().flat_map(|(c, _)| c.iter().copied()).collect();
            posts.retain(|(c, _)| !one_poster && !c.iter().any(|id| joined.contains(id)));
            posts.extend(sharing_whole);
        }
    }
    // one poster's rows are `same_poster_post`'s, unless they share parts
    if one_poster && !posts.iter().any(|(_, union)| union.overlap) {
        return Ok((None, false, HashSet::new()));
    }
    // the rows of the posts sharing parts and their crossposts: a poster's own rows that one of them is in make no
    // post of their own
    let mut claimed: HashSet<i64> = posts
        .iter()
        .filter(|(chosen, union)| chosen.len() > 1 && union.overlap)
        .flat_map(|(chosen, _)| chosen.iter().copied())
        .collect();
    let copies_claimed: Vec<i64> = copies.iter().filter(|(_, of)| claimed.contains(of)).map(|(n, _)| n.id).collect();
    claimed.extend(copies_claimed);
    for (chosen, union) in posts {
        let anchor_in = chosen.contains(&anchor) || copies.iter().any(|(n, of)| n.id == anchor && chosen.contains(of));
        if !anchor_in {
            continue;
        }
        // whether the post the row asking is in shares parts: then it's the answer, whatever its poster's rows say
        let shared = union.overlap;
        if chosen.len() < 2 || !union.one_upload() {
            return Ok((None, shared, claimed));
        }
        return Ok((
            post_of(kept.iter().map(|(n, _)| *n).filter(|n| chosen.contains(&n.id)).collect(), &copies, union),
            shared,
            claimed,
        ));
    }
    Ok((None, false, claimed))
}

/// The rows not `taken` that join `kept[seed]`, looked at again whenever one joins (a row can share a file only with
/// one that comes later): each fills the others' gaps, and they stay within `SPLIT_POST_DAYS` of each other.
fn grow_post<E>(
    load: &mut impl FnMut(&[i64])->std::result::Result<UnionFiles,E>,
    kept: &[(&Named, UnionFiles)],
    seed: usize,
    taken: &HashSet<i64>,
    shared_parts: bool,
) -> std::result::Result<(Vec<i64>, UnionFiles),E> {
    let mut chosen = vec![kept[seed].0.id];
    let mut union = load( &chosen)?;
    let mut span = kept[seed].0.date.map_or((0.0, 0.0), |d| (d, d));
    // a row whole on its own is an upload of its own: no row sharing parts with the post joins one that has it
    let mut whole = kept[seed].1.complete();
    let mut grew = true;
    while grew {
        grew = false;
        for (n, files) in kept {
            let Some(date) = n.date else { continue };
            if chosen.contains(&n.id) || taken.contains(&n.id) {
                continue;
            }
            let (lo, hi) = (span.0.min(date), span.1.max(date));
            // rows sharing parts only where the caller allows them
            if !shared_parts && union.shares_parts(files) {
                continue;
            }
            let shares = files.files.keys().any(|f| union.files.contains_key(f));
            if hi - lo > SPLIT_POST_DAYS || !shares || !union.fills_gaps(files, whole) {
                continue;
            }
            chosen.push(n.id);
            let grown = load( &chosen)?;
            // a row that would make the rest no one upload (a counter-less post of other files) stays out
            if !grown.one_upload() {
                chosen.pop();
                continue;
            }
            union = grown;
            span = (lo, hi);
            whole |= files.complete();
            grew = true;
        }
    }
    Ok((chosen, union))
}

/// A post of these rows, with what it stands for: its rows and their crossposts.
fn post_of(rows: Vec<&Named>, copies: &[(&Named, i64)], union: UnionFiles) -> Option<(SplitPost, Vec<i64>)> {
    let mut ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
    let mut groups: Vec<String> = rows.iter().map(|n| n.group.clone()).collect();
    groups.extend(copies.iter().filter(|(_, of)| ids.contains(of)).map(|(n, _)| n.group.clone()));
    groups.sort_unstable();
    groups.dedup();
    if groups.len() > SPLIT_POST_GROUPS {
        return None;
    }
    let summed: i64 = rows.iter().map(|n| n.bytes).sum();
    // rows sharing parts hold some bytes more than once: the files' sizes from their subjects, each by the share of
    // its parts there, or else the rows' bytes scaled by the parts there are to the parts they hold
    let size = match (union.overlap, union.subject_bytes(), union.parts_held) {
        (false, ..) => summed,
        (true, Some(bytes), _) => bytes,
        (true, None, held) if held > 0 => {
            (i128::from(summed) * i128::from(union.distinct_parts()) / i128::from(held)) as i64
        }
        (true, None, _) => summed,
    };
    ids.sort_unstable();
    let mut covers = ids.clone();
    covers.extend(copies.iter().filter(|(_, of)| ids.contains(of)).map(|(n, _)| n.id));
    Some((
        SplitPost {
            ids,
            groups,
            size,
            complete: union.complete(),
            lead: None,
            recovery: Vec::new(),
            beside: Vec::new(),
        },
        covers,
    ))
}

#[derive(Clone)]
pub(crate) struct StemCandidate {
    pub(crate) id: i64,
    pub(crate) group: String,
    pub(crate) bytes: i64,
    pub(crate) date: Option<f64>,
}

pub(crate) fn try_stem_subset<E>(load:&mut impl FnMut(&[i64])->std::result::Result<UnionFiles,E>, stem: &str, subset: &[StemCandidate]) -> std::result::Result<Option<SplitPost>,E> {
    if !(2..=LINKED_ROWS).contains(&subset.len()) {
        return Ok(None);
    }
    let dates: Vec<Option<f64>> = subset.iter().map(|c| c.date).collect();
    if !close_together(&dates) {
        return Ok(None);
    }
    let mut ids: Vec<i64> = subset.iter().map(|c| c.id).collect();
    let mut groups: Vec<String> = subset.iter().map(|c| c.group.clone()).collect();
    let size: i64 = subset.iter().map(|c| c.bytes).sum();
    ids.sort_unstable();
    groups.sort_unstable();
    groups.dedup();
    if groups.len() > SPLIT_POST_GROUPS {
        return Ok(None);
    }
    let files = load(&ids)?;
    if !files.one_stem_upload(stem) {
        return Ok(None);
    }
    Ok(Some(SplitPost {
        ids,
        groups,
        size,
        complete: files.complete(),
        lead: None,
        recovery: Vec::new(),
        beside: Vec::new(),
    }))
}

pub(super) fn close_together(dates: &[Option<f64>]) -> bool {
    let Some(dates) = dates.iter().copied().collect::<Option<Vec<f64>>>() else { return false };
    let (first, last) = dates.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    last - first <= SPLIT_POST_DAYS
}

/// A file across releases: what's expected of it, and which parts any of them has.
pub(super) struct UnionFile {
    pub(super) expected: Option<i64>,
    pub(super) seen: Vec<u8>,
    /// its subject, less the part number
    pub(super) subject: Option<String>,
}

/// Whether a file's subject (less its part number) ends with a byte count, as Nyuu and ngPost write the file's size:
/// `"Film.mkv" yEnc (/21040) 88246893816`. Two rows agreeing on such a subject agree on the file's name, article count
/// and size. Message-ids aren't compared: two uploads of one file this alike hold parts that stand in for each other.
pub(super) fn subject_has_size(subject: &str) -> bool {
    subject_size(subject).is_some()
}

/// The byte count a subject ends with, after its last `)` (`subject_has_size`).
pub(super) fn subject_size(subject: &str) -> Option<i64> {
    let tail = subject[subject.rfind(')')? + 1..].trim();
    if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tail.parse().ok()
}

/// Each file across these releases, by name, and what says whether they're one upload.
#[derive(Default)]
pub(crate) struct UnionFiles {
    pub(super) files: BTreeMap<String, UnionFile>,
    /// what each file says the post has
    pub(super) file_totals: BTreeSet<Option<i64>>,
    /// each release's filenames
    pub(super) rows: Vec<BTreeSet<String>>,
    /// two releases have a file they don't agree on, or a part of it both have under a subject without the file's size
    conflict: bool,
    /// two releases have a part of a file both agree on, its subject carrying the file's size: a post whose poster
    /// sent each article to some of its groups, soo a part can be in more than one row
    pub(super) overlap: bool,
    /// the parts every row holds, added up: more than `distinct_parts` where rows share parts
    parts_held: i64,
    /// whether any file carries a 0-indexed `[0/N]` or `[00/N]` counter
    zero_indexed: bool,
    /// each release's name
    pub(super) names: BTreeSet<String>,
}

impl UnionFiles {
    pub(crate) fn add_file(&mut self, filenames:&mut BTreeSet<String>, filename:String, expected:Option<i64>, seen:Vec<u8>, total:Option<i64>, subject:Option<String>) {
            self.file_totals.insert(total);
            self.parts_held += store::seen_count(&seen);
            self.zero_indexed |= subject.as_deref().is_some_and(crate::parser::has_zero_file_index);
            filenames.insert(filename.clone());
            let subject = subject.as_deref().map(|subject| subject_of_file(subject, expected));
            match self.files.entry(filename) {
                Entry::Vacant(file) => {
                    file.insert(UnionFile { expected, seen, subject });
                }
                Entry::Occupied(mut file) => {
                    let file = file.get_mut();
                    let disagree =
                        expected.is_none() || file.expected != expected || subject.is_none() || file.subject != subject;
                    self.conflict |= disagree;
                    // the same part of one file in two rows is the same article only where the subject names the
                    // file by its size as well: without it, two files of a name can't be told apart
                    let sized = !disagree && subject.as_deref().is_some_and(subject_has_size);
                    if store::merge_parts(&mut file.seen, &seen) {
                        self.overlap |= sized;
                        self.conflict |= !sized;
                    }
                }
            }
    }
    pub(crate) fn end_row(&mut self,name:Option<String>,filenames:BTreeSet<String>) {self.names.extend(name);self.rows.push(filenames);}
    pub(crate) fn permille(&self)->i64 {let (mut have,mut want)=(0i64,0i64);for f in self.files.values(){if let Some(n)=f.expected.filter(|n|*n>0){have+=store::seen_count(&f.seen).min(n);want+=n;}}if want==0{0}else{have*1000/want}}

    /// Whether the releases can be parts of one upload. They say the same of every file they share, each shares one
    /// with the rest, and no part is in two of them unless its subject names the file by its size (`overlap`: the poster
    /// sent the article to more than one of the groups).
    fn one_upload(&self) -> bool {
        let mut totals = self.file_totals.iter();
        let total_fits = match (totals.next(), totals.next()) {
            (Some(Some(total)), None) => self.files.len() as i64 <= *total + i64::from(self.zero_indexed),
            // no count to go by, soo every release has the same files
            (Some(None), None) => self.rows.windows(2).all(|pair| pair[0] == pair[1]),
            _ => false,
        };
        !self.conflict && total_fits && self.linked()
    }

    pub(crate) fn one_stem_upload(&self, stem: &str) -> bool {
        if self.conflict || self.overlap || self.files.is_empty() || self.file_totals.contains(&None) {
            return false;
        }
        if !self.files.keys().all(|f| post_stem(f).eq_ignore_ascii_case(stem)) {
            return false;
        }
        let non_null: Vec<i64> = self.file_totals.iter().copied().flatten().collect();
        match non_null.as_slice() {
            [total] if *total >= 2 => {
                let want = *total + i64::from(self.zero_indexed);
                self.files.len() >= 2 && (self.files.len() as i64) <= want
            }
            _ => false,
        }
    }

    /// Whether `other` holds the very parts this does of the very same files: a crosspost, not a part of the post.
    fn same_parts(&self, other: &UnionFiles) -> bool {
        self.files.len() == other.files.len()
            && self.files.iter().all(|(name, f)| {
                other.files.get(name).is_some_and(|o| {
                    o.expected == f.expected && o.subject == f.subject && store::same_parts(&o.seen, &f.seen)
                })
            })
    }

    /// Whether `other` only adds what this lacks: every file both have is the same file under the same subject, and
    /// they say the same of how many files the post has. No part is in both, except under a subject that ends with a
    /// byte count (the same name, part count and size: `subject_has_size`), where neither `other` nor a row already
    /// in this (`whole`) is the whole post on its own, and where `other` still adds a part this lacks: a poster can
    /// send each article to some of the post's groups, soo its rows share parts, but a whole upload is its own post.
    fn fills_gaps(&self, other: &UnionFiles, whole: bool) -> bool {
        if other.conflict || self.conflict {
            return false;
        }
        let mut totals = self.file_totals.union(&other.file_totals);
        if totals.next().is_some() && totals.next().is_some() {
            return false;
        }
        let mut shared = false;
        let mut adds = false;
        let agrees = other.files.iter().all(|(name, o)| match self.files.get(name) {
            None => {
                adds = true;
                true
            }
            Some(f) => {
                let same =
                    f.expected.is_some() && f.expected == o.expected && f.subject.is_some() && f.subject == o.subject;
                let both = store::parts_overlap(&f.seen, &o.seen);
                adds |= store::adds_parts(&f.seen, &o.seen, o.expected);
                if both {
                    shared = true;
                    return same && f.subject.as_deref().is_some_and(subject_has_size);
                }
                same
            }
        });
        agrees && (!shared || (adds && !whole && !other.complete()))
    }

    /// The bytes of the parts there are, by each file's size in its subject (`subject_size`) times the share of its
    /// expected parts there. None when a file's subject doesn't give its size.
    fn subject_bytes(&self) -> Option<i64> {
        self.files.values().try_fold(0i64, |sum, f| {
            let size = subject_size(f.subject.as_deref()?)?;
            let expected = f.expected.filter(|e| *e > 0)?;
            let have = store::seen_count(&f.seen).min(expected);
            sum.checked_add(i64::try_from(i128::from(size) * i128::from(have) / i128::from(expected)).ok()?)
        })
    }

    /// Whether `other` has a part of a file this has too.
    fn shares_parts(&self, other: &UnionFiles) -> bool {
        self.files
            .iter()
            .any(|(name, f)| other.files.get(name).is_some_and(|o| store::parts_overlap(&f.seen, &o.seen)))
    }

    /// The parts there are, each once however many rows hold it.
    fn distinct_parts(&self) -> i64 {
        self.files.values().map(|f| store::seen_count(&f.seen)).sum()
    }

    fn linked(&self) -> bool {
        let Some((first, rest)) = self.rows.split_first() else { return false };
        let mut reached = first.clone();
        let mut left: Vec<&BTreeSet<String>> = rest.iter().collect();
        while let Some(at) = left.iter().position(|row| !row.is_disjoint(&reached)) {
            reached.extend(left.swap_remove(at).iter().cloned());
        }
        left.is_empty()
    }

    /// Whether every file is there with exactly its parts, whatever the subjects say the post has: a linked upload's
    /// files each came with a subject of their own.
    pub(crate) fn complete_by_files(&self) -> bool {
        !self.files.is_empty()
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
    }

    /// Whether every file the post says it has is there with exactly its parts, or every file there is whole and they
    /// are one video named as the post beside side files (`store::one_video_named`): the counter can count a
    /// `thumbnail.jpg` that came as a row of its own.
    pub(crate) fn complete(&self) -> bool {
        !self.files.is_empty()
            && self.files.values().all(|f| f.expected.is_some_and(|want| store::is_exactly(&f.seen, want)))
            && self.every_file_here()
    }

    /// Whether every file the post's counter says it has is here, whole or not (`complete` also wants them whole).
    pub(super) fn every_file_here(&self) -> bool {
        if self.file_totals.len() > 1 {
            return false;
        }
        let total = self.file_totals.iter().copied().flatten().next_back();
        total.is_none_or(|total| self.files.len() as i64 == total + i64::from(self.zero_indexed))
            || store::one_video_named(self.names.iter().map(String::as_str), self.files.keys().map(String::as_str))
    }
}

/// A subject without the number that changes from part to part: the N in `(N/M)`, or in `[N/M]` for a post where
/// `(1/1)` says each article is a whole file, soo the bracket counter is the part number (`parser::remap_bracket_parts`).
fn subject_of_file(subject: &str, expected: Option<i64>) -> String {
    let mut subject = subject.to_string();
    let mut one_article_files = false;
    for (open, _) in subject.rmatch_indices('(') {
        let rest = &subject[open + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let counter = rest[digits..].trim_start();
        if digits > 0 && counter.starts_with('/') {
            one_article_files = counter[1..].trim_start().starts_with("1)");
            subject = format!("{}({counter}", &subject[..open]);
            break;
        }
    }
    if one_article_files && let Some(end) = expected.and_then(|parts| subject.find(&format!("/{parts}]"))) {
        let number = subject[..end].trim_end_matches(|c: char| c.is_ascii_digit());
        if number.ends_with('[') && number.len() < end {
            return format!("{number}{}", &subject[end..]);
        }
    }
    subject
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(parts:&[Vec<i64>],sized:bool,anchor:i64)->Option<SplitPost> {
        let rows=parts.iter().enumerate().map(|(i,parts)|Named {id:i as i64+1,group:format!("owned.group.{i}"),bytes:parts.len() as i64*250,date:Some(1.0),poster:Some(format!("actual-poster-{i}"))}).collect::<Vec<_>>();
        let same=rows.iter().filter(|row|row.id==anchor).cloned().collect::<Vec<_>>();
        exact_named(anchor,&same,&rows,&mut |ids|->Result<UnionFiles,std::convert::Infallible> {
            let subject=if sized{"data.bin yEnc (/4) 1000"}else{"data.bin yEnc (/4)"};
            let mut union=UnionFiles::default();for id in ids {let mut seen=Vec::new();for part in &parts[*id as usize-1]{store::insert_part(&mut seen,*part);}
                let mut names=BTreeSet::new();union.add_file(&mut names,"data.bin".into(),Some(4),seen,Some(1),Some(subject.into()));union.end_row(Some("Owned.Post".into()),names);
            }Ok(union)
        }).unwrap()
    }
    #[test]
    fn complementary_slices_and_crossposts_agree_while_full_twins_stay_separate() {
        let slices=[vec![1,2],vec![3,4],vec![1,2]];
        for anchor in 1..=3 {let post=resolve(&slices,true,anchor).expect("real complementary group slices form one post");assert_eq!(post.ids,[1,2]);assert_eq!(post.groups.len(),3);assert!(post.complete);assert_eq!(post.size,1000);}
        for anchor in 1..=2 {assert!(resolve(&[vec![1,2,3,4],vec![1,2,3,4]],true,anchor).is_none(),"two complete reposts never become a complementary upload");}
    }
    #[test]
    fn overlapping_slices_require_real_size_subject_and_additions() {
        let slices=[vec![1,2,3],vec![2,3,4]];
        assert!(resolve(&slices,false,1).is_none(),"a same filename/part count alone cannot establish one upload");
        let post=resolve(&slices,true,1).unwrap();assert_eq!(post.ids,[1,2]);assert!(post.complete);assert_eq!(post.size,1000,"overlapping bytes count once from the real subject size");
    }
}
