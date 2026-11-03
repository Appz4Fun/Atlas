//! Release merging, sequential file tiling, PAR2 hub-and-spoke bridge,
//! real-name deobfuscation, and two-layer veto-aware DSU graph partitioning.

use std::collections::HashMap;

use md5::Digest;

/// First article payload of a candidate file run, used for 16 KiB MD5 tie-breaking
/// and archive header probing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleHead {
    pub file_index: usize,
    pub payload: Vec<u8>,
}

/// A candidate file run within an upload stream or newsgroup slice.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateFileRun {
    pub filename: String,
    pub group: String,
    pub poster: String,
    pub size: u64,
    pub yenc_total: Option<i64>,
    pub yenc_part_size: Option<u64>,
    pub file_index: Option<i64>,
    pub rar_volume_idx: Option<u32>,
    pub archive_format: Option<String>,
    pub member_id: Option<String>,
    pub byte_offset: u64,
    pub t_start: f64,
    pub t_end: f64,
    pub file_crc: Option<u32>,
    pub md5_16k: Option<[u8; 16]>,
    pub full_md5: Option<[u8; 16]>,
    pub unpacked_crc32: Option<u32>,
    pub creation_salt: Option<Vec<u8>>,
    pub is_obfuscated: bool,
}

impl CandidateFileRun {
    /// Check whether this run's byte range overlaps with another run of the same member.
    pub fn byte_range_overlaps(&self, other: &Self) -> bool {
        let self_start = self.byte_offset;
        let self_end = self.byte_offset.saturating_add(self.size);
        let other_start = other.byte_offset;
        let other_end = other.byte_offset.saturating_add(other.size);
        self_start < other_end && other_start < self_end
    }

    /// Check if part sizes permit an LCM grid alignment.
    pub fn is_lcm_compatible(&self, other: &Self) -> bool {
        match (self.yenc_part_size, other.yenc_part_size) {
            (Some(s1), Some(s2)) => s1 > 0 && s2 > 0,
            _ => false,
        }
    }
}

/// Options controlling sequential arrival window evaluation.
#[derive(Debug, Clone)]
pub struct JoinOptions {
    pub avg_width: f64,
    pub posting_rate: f64,
    pub max_gap_seconds: f64,
    pub lower_scale: f64,
    pub lower_offset: f64,
}

impl Default for JoinOptions {
    fn default() -> Self {
        Self {
            avg_width: 750_000.0,
            posting_rate: 5_000_000.0,
            max_gap_seconds: 2.0,
            lower_scale: -1.2,
            lower_offset: -0.5,
        }
    }
}

/// A release assembled from one or more candidate file runs.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedRelease {
    pub name: String,
    pub runs: Vec<CandidateFileRun>,
    pub total_size: u64,
    pub complete: bool,
    pub display_name: Option<String>,
    pub is_obfuscated: bool,
}

pub struct ReleaseAssembler;

impl ReleaseAssembler {
    /// Join candidate file runs into coherent releases using the end-to-start arrival window:
    /// Delta t = t_start(F_{k+1}) - t_end(F_k) in [lower_scale * (w / r) + lower_offset, max_gap_seconds]
    pub fn join_runs(runs: &[CandidateFileRun], options: Option<&JoinOptions>) -> Vec<MergedRelease> {
        if runs.is_empty() {
            return Vec::new();
        }

        let opts = options.cloned().unwrap_or_default();
        let lower_bound = opts.lower_scale * (opts.avg_width / opts.posting_rate) + opts.lower_offset;
        let upper_bound = opts.max_gap_seconds;

        // Sort runs chronologically by start time
        let mut sorted = runs.to_vec();
        sorted.sort_by(|a, b| a.t_start.partial_cmp(&b.t_start).unwrap_or(std::cmp::Ordering::Equal));

        let mut releases: Vec<MergedRelease> = Vec::new();
        let mut current_runs: Vec<CandidateFileRun> = Vec::new();

        for run in sorted {
            if let Some(last) = current_runs.last() {
                let delta_t = run.t_start - last.t_end;
                let within_window = delta_t >= lower_bound && delta_t <= upper_bound;
                let can_group = current_runs
                    .iter()
                    .all(|existing| IntraReleaseGrouping::can_group(existing, &run).is_ok());

                if within_window && can_group {
                    current_runs.push(run);
                } else {
                    let merged = Self::create_merged_release(std::mem::take(&mut current_runs));
                    releases.push(merged);
                    current_runs.push(run);
                }
            } else {
                current_runs.push(run);
            }
        }

        if !current_runs.is_empty() {
            releases.push(Self::create_merged_release(current_runs));
        }

        releases
    }

    fn create_merged_release(runs: Vec<CandidateFileRun>) -> MergedRelease {
        let total_size: u64 = runs.iter().map(|r| r.size).sum();
        let name = runs.first().map(|r| r.filename.clone()).unwrap_or_else(|| "unnamed".to_string());
        let any_obfuscated = runs.iter().any(|r| r.is_obfuscated);
        MergedRelease {
            name,
            runs,
            total_size,
            complete: true,
            display_name: None,
            is_obfuscated: any_obfuscated,
        }
    }
}

/// Result of PAR2 hub-and-spoke candidate resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Par2BridgeResult {
    pub resolved_count: usize,
    pub total_candidates: usize,
    pub success: bool,
}

pub struct Par2Bridge;

impl Par2Bridge {
    /// Resolve unlabelled obfuscated candidate runs against PAR2 index packets.
    ///
    /// 1. sizeChain matching: 1-to-1 match on unique file sizes.
    /// 2. 16 KiB MD5 tie-breaker: computes MD5 over first 16 KiB of article 1.
    /// 3. RAR5 volume header vint parsing fallback when 16 KiB prefixes collide.
    pub fn resolve(
        par2_data: &[u8],
        candidate_runs: &mut [CandidateFileRun],
        first_articles: &[ArticleHead],
    ) -> Par2BridgeResult {
        if par2_data.is_empty() || candidate_runs.is_empty() {
            return Par2BridgeResult {
                resolved_count: 0,
                total_candidates: candidate_runs.len(),
                success: false,
            };
        }

        let inv = crate::par2::inventory(par2_data);
        if inv.files.is_empty() {
            return Par2BridgeResult {
                resolved_count: 0,
                total_candidates: candidate_runs.len(),
                success: false,
            };
        }

        let mut resolved = 0;
        let mut matched_file_ids = std::collections::HashSet::new();

        // Step 1: Unique sizeChain matching
        let mut par2_size_counts = HashMap::new();
        for f in &inv.files {
            *par2_size_counts.entry(f.size).or_insert(0usize) += 1;
        }

        let mut candidate_size_counts = HashMap::new();
        for r in candidate_runs.iter() {
            *candidate_size_counts.entry(r.size).or_insert(0usize) += 1;
        }

        for run in candidate_runs.iter_mut() {
            if candidate_size_counts.get(&run.size) == Some(&1)
                && par2_size_counts.get(&run.size) == Some(&1)
                && let Some(f) = inv.files.iter().find(|pf| pf.size == run.size)
            {
                run.filename = f.name.clone();
                run.full_md5 = Some(f.md5);
                run.md5_16k = Some(f.md5_16k);
                if let Some(crc) = f.crc32 {
                    run.file_crc = Some(crc);
                }
                run.is_obfuscated = false;
                matched_file_ids.insert(f.file_id);
                resolved += 1;
            }
        }

        // Precompute (size, md5_16k) frequencies to reject ambiguous 16 KiB prefix matches
        let mut par2_prefix_counts: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
        for f in &inv.files {
            *par2_prefix_counts.entry((f.size, f.md5_16k)).or_insert(0) += 1;
        }

        let mut candidate_heads: Vec<Option<(&ArticleHead, [u8; 16])>> = Vec::with_capacity(candidate_runs.len());
        let mut candidate_prefix_counts: HashMap<(u64, [u8; 16]), usize> = HashMap::new();
        for (i, run) in candidate_runs.iter().enumerate() {
            if run.full_md5.is_some() {
                candidate_heads.push(None);
                continue;
            }
            let head = first_articles
                .iter()
                .find(|a| a.file_index == i || a.file_index == run.file_index.unwrap_or(-1) as usize);
            if let Some(article_head) = head {
                let md5_bytes = if article_head.payload.len() >= 16384 {
                    &article_head.payload[..16384]
                } else {
                    &article_head.payload
                };
                let head_md5: [u8; 16] = md5::Md5::digest(md5_bytes).into();
                *candidate_prefix_counts.entry((run.size, head_md5)).or_insert(0) += 1;
                candidate_heads.push(Some((article_head, head_md5)));
            } else {
                candidate_heads.push(None);
            }
        }

        // Step 2 & 3: 16 KiB MD5 tie-breaker and RAR5 volume vint fallback for equal sizes
        for (run, head_entry) in candidate_runs.iter_mut().zip(candidate_heads) {
            if run.full_md5.is_some() {
                continue;
            }

            if let Some((article_head, head_md5)) = head_entry {
                let unique_prefix = par2_prefix_counts.get(&(run.size, head_md5)) == Some(&1)
                    && candidate_prefix_counts.get(&(run.size, head_md5)) == Some(&1);

                let prefix_match = if unique_prefix {
                    inv.files.iter().find(|pf| {
                        pf.size == run.size && pf.md5_16k == head_md5 && !matched_file_ids.contains(&pf.file_id)
                    })
                } else if let Some(expected_crc) = run.file_crc {
                    let mut crc_matches = inv.files.iter().filter(|pf| {
                        pf.size == run.size
                            && pf.md5_16k == head_md5
                            && pf.crc32 == Some(expected_crc)
                            && !matched_file_ids.contains(&pf.file_id)
                    });
                    match (crc_matches.next(), crc_matches.next()) {
                        (Some(f), None) => Some(f),
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(f) = prefix_match {
                    run.filename = f.name.clone();
                    run.full_md5 = Some(f.md5);
                    run.md5_16k = Some(f.md5_16k);
                    if let Some(crc) = f.crc32 {
                        run.file_crc = Some(crc);
                    }
                    run.is_obfuscated = false;
                    matched_file_ids.insert(f.file_id);
                    resolved += 1;
                    continue;
                }

                // Step 3: RAR5 volume number vint fallback
                if let Some(vol_num) = Self::parse_rar5_volume_num(&article_head.payload) {
                    run.rar_volume_idx = Some(vol_num as u32);
                    let vol_p0 = format!("part{:03}.", vol_num);
                    let vol_p1 = format!("part{:02}.", vol_num);
                    let vol_p2 = format!("part{}.", vol_num);
                    let vol_p3 = format!(".r{:02}", vol_num.saturating_sub(1));
                    let mut vol_matches = inv.files.iter().filter(|pf| {
                        let lower = pf.name.to_ascii_lowercase();
                        pf.size == run.size
                            && !matched_file_ids.contains(&pf.file_id)
                            && (lower.contains(&vol_p0)
                                || lower.contains(&vol_p1)
                                || lower.contains(&vol_p2)
                                || lower.contains(&vol_p3))
                    });
                    if let (Some(f), None) = (vol_matches.next(), vol_matches.next()) {
                        run.filename = f.name.clone();
                        run.full_md5 = Some(f.md5);
                        run.md5_16k = Some(f.md5_16k);
                        if let Some(crc) = f.crc32 {
                            run.file_crc = Some(crc);
                        }
                        run.is_obfuscated = false;
                        matched_file_ids.insert(f.file_id);
                        resolved += 1;
                    }
                }
            }
        }

        Par2BridgeResult {
            resolved_count: resolved,
            total_candidates: candidate_runs.len(),
            success: resolved == candidate_runs.len(),
        }
    }

    /// Parse RAR5 volume number from main archive header block.
    pub fn parse_rar5_volume_num(payload: &[u8]) -> Option<u64> {
        const RAR5_SIG: &[u8; 8] = b"Rar!\x1A\x07\x01\x00";
        if !payload.starts_with(RAR5_SIG) {
            return None;
        }

        let mut pos = 8;
        if payload.len() < pos + 4 {
            return None;
        }
        pos += 4; // Skip CRC32

        // Read header size vint
        let (_size, next_pos) = Self::read_vint(payload, pos)?;
        pos = next_pos;

        // Read header type vint (Type 1 = Main archive header)
        let (htype, next_pos) = Self::read_vint(payload, pos)?;
        pos = next_pos;
        if htype != 1 {
            return None;
        }

        // Read common header flags vint
        let (header_flags, next_pos) = Self::read_vint(payload, pos)?;
        pos = next_pos;

        // Common flag 0x0001 = Extra area size present
        if (header_flags & 0x0001) != 0 {
            let (_extra_size, next_pos) = Self::read_vint(payload, pos)?;
            pos = next_pos;
        }
        // Common flag 0x0002 = Data area size present
        if (header_flags & 0x0002) != 0 {
            let (_data_size, next_pos) = Self::read_vint(payload, pos)?;
            pos = next_pos;
        }

        // Read main header archive flags vint
        let (archive_flags, next_pos) = Self::read_vint(payload, pos)?;
        pos = next_pos;

        // Archive flag 0x0002 = Volume number field present (0-based for volumes >= 2; absent on volume 1)
        if (archive_flags & 0x0002) != 0 {
            let (vol_num, _) = Self::read_vint(payload, pos)?;
            Some(vol_num + 1)
        } else {
            Some(1)
        }
    }

    fn read_vint(bytes: &[u8], mut pos: usize) -> Option<(u64, usize)> {
        let mut val = 0u64;
        let mut shift = 0;
        while pos < bytes.len() {
            let b = bytes[pos];
            pos += 1;
            val |= ((b & 0x7F) as u64) << shift;
            if (b & 0x80) == 0 {
                return Some((val, pos));
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
        None
    }
}

/// Deobfuscated filename mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeobfuscatedName {
    pub original_name: String,
    pub canonical_name: String,
    pub is_clean: bool,
}

pub struct Deobfuscator;

impl Deobfuscator {
    /// Extract real filenames from Postie FileDesc and NewsUP rename packets,
    /// sanitizing path traversal, stripping NULs, and clamping to 255 chars.
    pub fn resolve_names(par2_data: &[u8]) -> Vec<DeobfuscatedName> {
        let inv = crate::par2::inventory(par2_data);
        let mut results = Vec::new();

        for f in inv.files {
            let clean = Self::sanitize_name(&f.name);
            let looks_random = Self::looks_random(&clean);
            results.push(DeobfuscatedName {
                original_name: f.name.clone(),
                canonical_name: clean,
                is_clean: !looks_random,
            });
        }

        results
    }

    /// Sanitize filename: strip path traversal, NUL bytes, clamp to 255 chars.
    pub fn sanitize_name(name: &str) -> String {
        let without_nulls: String = name.chars().filter(|c| *c != '\0').collect();

        let leaf = without_nulls
            .replace('\\', "/")
            .split('/')
            .rfind(|part| *part != ".." && *part != "." && !part.is_empty())
            .unwrap_or("")
            .to_string();

        if leaf.chars().count() > 255 {
            leaf.chars().take(255).collect()
        } else {
            leaf
        }
    }

    /// Check if filename looks random / obfuscated.
    pub fn looks_random(name: &str) -> bool {
        let stem = match name.rfind('.') {
            Some(idx) => &name[..idx],
            None => name,
        };

        if stem.len() >= 16 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
            return true;
        }

        let has_vowels = stem
            .chars()
            .any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'));
        let has_separators = stem.chars().any(|c| matches!(c, '.' | '-' | '_' | ' '));
        if stem.len() >= 20 && !has_separators && !has_vowels {
            return true;
        }

        false
    }
}

/// Result of a VetoDsu union operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult<E> {
    Merged,
    AlreadyMerged,
    Vetoed(E),
}

/// Generic Disjoint Set Union (DSU) data structure with custom veto closures.
pub struct VetoDsu<T> {
    parent: Vec<usize>,
    rank: Vec<usize>,
    members: Vec<Vec<usize>>,
    pub elements: Vec<T>,
}

impl<T> VetoDsu<T> {
    pub fn new(elements: Vec<T>) -> Self {
        let n = elements.len();
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
            members: (0..n).map(|idx| vec![idx]).collect(),
            elements,
        }
    }

    pub fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut curr = i;
        while curr != root {
            let next = self.parent[curr];
            self.parent[curr] = root;
            curr = next;
        }
        root
    }

    pub fn union<F, E>(&mut self, i: usize, j: usize, veto: F) -> MergeResult<E>
    where
        F: Fn(&T, &T) -> Result<(), E>,
    {
        let root_i = self.find(i);
        let root_j = self.find(j);

        if root_i == root_j {
            return MergeResult::AlreadyMerged;
        }

        for &a in &self.members[root_i] {
            for &b in &self.members[root_j] {
                if let Err(e) = veto(&self.elements[a], &self.elements[b]) {
                    return MergeResult::Vetoed(e);
                }
            }
        }

        match self.rank[root_i].cmp(&self.rank[root_j]) {
            std::cmp::Ordering::Less => {
                self.parent[root_i] = root_j;
                let moved = std::mem::take(&mut self.members[root_i]);
                self.members[root_j].extend(moved);
            }
            std::cmp::Ordering::Greater => {
                self.parent[root_j] = root_i;
                let moved = std::mem::take(&mut self.members[root_j]);
                self.members[root_i].extend(moved);
            }
            std::cmp::Ordering::Equal => {
                self.parent[root_j] = root_i;
                self.rank[root_i] += 1;
                let moved = std::mem::take(&mut self.members[root_j]);
                self.members[root_i].extend(moved);
            }
        }

        MergeResult::Merged
    }
}

/// Reasons why sibling file runs cannot be merged into the same release container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntraReleaseVeto {
    DuplicateVolumeIndex,
    OverlappingByteOffsets,
    IncompatibleContainer,
    PartSizeMismatch,
    DiscrepantYencTotalForSameFile,
}

pub struct IntraReleaseGrouping;

impl IntraReleaseGrouping {
    /// Evaluates whether sibling file runs can merge into the same release container.
    ///
    /// Invariant: NEVER vetoes on differing yenc_total or differing file_crc across distinct files!
    pub fn can_group(a: &CandidateFileRun, b: &CandidateFileRun) -> Result<(), IntraReleaseVeto> {
        // VETO 1: Duplicate volume index (e.g. two part01.rar files)
        if let (Some(va), Some(vb)) = (a.rar_volume_idx, b.rar_volume_idx)
            && va == vb
        {
            return Err(IntraReleaseVeto::DuplicateVolumeIndex);
        }

        // VETO 2: Overlapping byte offsets for same member file
        if a.member_id.is_some() && a.member_id == b.member_id && a.byte_range_overlaps(b) {
            return Err(IntraReleaseVeto::OverlappingByteOffsets);
        }

        // VETO 3: Incompatible container formats
        if let (Some(fa), Some(fb)) = (&a.archive_format, &b.archive_format)
            && fa != fb
        {
            return Err(IntraReleaseVeto::IncompatibleContainer);
        }

        // VETO 4: Part size mismatch for same logical member
        if a.member_id.is_some()
            && a.member_id == b.member_id
            && let (Some(ps_a), Some(ps_b)) = (a.yenc_part_size, b.yenc_part_size)
            && ps_a != ps_b
        {
            return Err(IntraReleaseVeto::PartSizeMismatch);
        }

        // VETO 5: Discrepant yEnc total for the SAME file index
        if let (Some(idx_a), Some(idx_b)) = (a.file_index, b.file_index)
            && idx_a == idx_b
            && let (Some(ta), Some(tb)) = (a.yenc_total, b.yenc_total)
            && ta != tb
        {
            return Err(IntraReleaseVeto::DiscrepantYencTotalForSameFile);
        }

        Ok(())
    }
}

/// A release candidate evaluated during Layer B cross-release deduplication.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseCandidate {
    pub name: String,
    pub full_md5: Option<[u8; 16]>,
    pub size: u64,
    pub unpacked_crc32: Option<u32>,
    pub yenc_total: Option<i64>,
    pub creation_salt: Option<Vec<u8>>,
    pub segment_uid: Option<[u8; 16]>,
    pub lcm_matched_intervals: usize,
    pub yenc_part_size: Option<u64>,
}

impl ReleaseCandidate {
    pub fn is_lcm_compatible(&self, other: &Self) -> bool {
        self.lcm_matched_intervals >= 4
            || other.lcm_matched_intervals >= 4
            || (self.yenc_part_size.is_some()
                && other.yenc_part_size.is_some()
                && self.yenc_part_size != other.yenc_part_size)
    }
}

/// Reasons why two distinct releases cannot be deduplicated as identical Twins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterReleaseVeto {
    ConflictingFullMd5,
    DifferingUnpackedCrc32,
    ConflictingYencPartsNonLcm,
    ConflictingCreationSalt,
}

/// Partition assignment distinguishing byte-identical Twins from alternative remux/repack Alts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasePartition {
    Twin,
    Alt,
    Distinct,
}

pub struct InterReleaseDeduplication;

impl InterReleaseDeduplication {
    /// Evaluates whether two distinct releases represent byte-identical duplicate reposts (Twins).
    pub fn can_deduplicate_as_twin(
        a: &ReleaseCandidate,
        b: &ReleaseCandidate,
    ) -> Result<(), InterReleaseVeto> {
        // VETO 1: Conflicting full file cryptographic hashes
        if let (Some(ha), Some(hb)) = (&a.full_md5, &b.full_md5)
            && ha != hb
        {
            return Err(InterReleaseVeto::ConflictingFullMd5);
        }

        // VETO 2: Conflicting unpacked member CRC32s
        if let (Some(ca), Some(cb)) = (a.unpacked_crc32, b.unpacked_crc32)
            && ca != cb
        {
            return Err(InterReleaseVeto::DifferingUnpackedCrc32);
        }

        // VETO 3: Conflicting yEnc total parts on non-LCM grid
        if let (Some(ta), Some(tb)) = (a.yenc_total, b.yenc_total)
            && ta != tb
            && !a.is_lcm_compatible(b)
        {
            return Err(InterReleaseVeto::ConflictingYencPartsNonLcm);
        }

        // VETO 4: Conflicting creation salts
        if let (Some(sa), Some(sb)) = (&a.creation_salt, &b.creation_salt)
            && sa != sb
        {
            return Err(InterReleaseVeto::ConflictingCreationSalt);
        }

        Ok(())
    }

    /// Partition releases into Twin (byte-identical repost), Alt (remux/repack/encode), or Distinct.
    pub fn partition_releases(a: &ReleaseCandidate, b: &ReleaseCandidate) -> ReleasePartition {
        if Self::can_deduplicate_as_twin(a, b).is_ok() {
            let md5_match = match (&a.full_md5, &b.full_md5) {
                (Some(ha), Some(hb)) => ha == hb && a.size == b.size,
                _ => false,
            };
            let lcm_match = a.lcm_matched_intervals >= 4 || b.lcm_matched_intervals >= 4;

            if md5_match || lcm_match {
                return ReleasePartition::Twin;
            }
        }

        // Check Alt proof: differing container hash/repost, but identical inner media CRC32 or SegmentUID
        let inner_crc_match = match (a.unpacked_crc32, b.unpacked_crc32) {
            (Some(ca), Some(cb)) => ca == cb,
            _ => false,
        };
        let segment_uid_match = match (&a.segment_uid, &b.segment_uid) {
            (Some(ua), Some(ub)) => ua == ub,
            _ => false,
        };

        if inner_crc_match || segment_uid_match {
            ReleasePartition::Alt
        } else {
            ReleasePartition::Distinct
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_start_window_join() {
        let run1 = CandidateFileRun {
            filename: "part01.rar".into(),
            group: "alt.binaries.movies".into(),
            poster: "anon@news".into(),
            size: 50_000_000,
            yenc_total: Some(2),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 100.0,
            t_end: 110.0,
            file_crc: Some(0x11111111),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        let run2 = CandidateFileRun {
            filename: "part02.rar".into(),
            group: "alt.binaries.test".into(), // different group!
            poster: "anon@news".into(),
            size: 50_000_000,
            yenc_total: Some(2),
            yenc_part_size: Some(750_000),
            file_index: Some(2),
            rar_volume_idx: Some(2),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 110.15, // Gap: +0.15s
            t_end: 120.0,
            file_crc: Some(0x22222222), // Distinct CRC!
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        let merged = ReleaseAssembler::join_runs(&[run1, run2], None);
        assert_eq!(merged.len(), 1, "should join across groups within +2.0s gap");
        assert_eq!(merged[0].runs.len(), 2);
    }

    #[test]
    fn intra_release_no_veto_on_differing_crc_or_parts() {
        let run1 = CandidateFileRun {
            filename: "vol01.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: Some(0xAAAA1111),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        // Tail volume has differing part total (12 vs 50) and distinct CRC
        let run2 = CandidateFileRun {
            filename: "vol02.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 12_000_000,
            yenc_total: Some(12),
            yenc_part_size: Some(750_000),
            file_index: Some(2),
            rar_volume_idx: Some(2),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 1.1,
            t_end: 1.5,
            file_crc: Some(0xBBBB2222),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        assert!(IntraReleaseGrouping::can_group(&run1, &run2).is_ok());
    }

    #[test]
    fn intra_release_veto_on_duplicate_volume() {
        let run1 = CandidateFileRun {
            filename: "part01.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: Some(0x1),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        let run2 = CandidateFileRun {
            filename: "part01_dup.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1), // duplicate volume index 1!
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 1.1,
            t_end: 2.0,
            file_crc: Some(0x2),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };

        let res = IntraReleaseGrouping::can_group(&run1, &run2);
        assert_eq!(res, Err(IntraReleaseVeto::DuplicateVolumeIndex));
    }

    #[test]
    fn inter_release_twin_vs_alt() {
        let rel1 = ReleaseCandidate {
            name: "Release.1080p".into(),
            full_md5: Some([1; 16]),
            size: 100_000_000,
            unpacked_crc32: Some(0x12345678),
            yenc_total: Some(100),
            creation_salt: Some(vec![1, 2, 3]),
            segment_uid: Some([10; 16]),
            lcm_matched_intervals: 0,
            yenc_part_size: Some(750_000),
        };

        // Identical full MD5 + size = Twin
        let rel2 = ReleaseCandidate {
            name: "Release.1080p.Repost".into(),
            full_md5: Some([1; 16]),
            size: 100_000_000,
            unpacked_crc32: Some(0x12345678),
            yenc_total: Some(100),
            creation_salt: Some(vec![1, 2, 3]),
            segment_uid: Some([10; 16]),
            lcm_matched_intervals: 0,
            yenc_part_size: Some(750_000),
        };
        assert_eq!(InterReleaseDeduplication::partition_releases(&rel1, &rel2), ReleasePartition::Twin);

        // Differing container hash, but same inner CRC = Alt
        let rel3 = ReleaseCandidate {
            name: "Release.1080p.Remux".into(),
            full_md5: Some([2; 16]),
            size: 95_000_000,
            unpacked_crc32: Some(0x12345678),
            yenc_total: Some(95),
            creation_salt: Some(vec![4, 5, 6]),
            segment_uid: Some([10; 16]),
            lcm_matched_intervals: 0,
            yenc_part_size: Some(750_000),
        };
        assert_eq!(InterReleaseDeduplication::partition_releases(&rel1, &rel3), ReleasePartition::Alt);
    }

    #[test]
    fn join_runs_rejects_non_adjacent_duplicate_volume() {
        let base = CandidateFileRun {
            filename: "part01.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 10.0,
            t_end: 20.0,
            file_crc: Some(1),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };
        let mut vol2 = base.clone();
        vol2.filename = "part02.rar".into();
        vol2.file_index = Some(2);
        vol2.rar_volume_idx = Some(2);
        vol2.t_start = 20.1;
        vol2.t_end = 30.0;

        let mut vol1_dup = base.clone();
        vol1_dup.filename = "part01_dup.rar".into();
        vol1_dup.file_index = Some(3);
        vol1_dup.rar_volume_idx = Some(1);
        vol1_dup.t_start = 30.1;
        vol1_dup.t_end = 40.0;

        let releases = ReleaseAssembler::join_runs(&[base, vol2, vol1_dup], None);
        assert_eq!(releases.len(), 2, "runs 1, 2, 1 must split before the second volume 1");
        assert_eq!(releases[0].runs.len(), 2);
        assert_eq!(releases[1].runs.len(), 1);
    }

    #[test]
    fn veto_dsu_enforces_constraints_across_all_component_members() {
        let base = CandidateFileRun {
            filename: "part01.rar".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(1),
            rar_volume_idx: Some(1),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        };
        let mut vol2 = base.clone();
        vol2.rar_volume_idx = Some(2);
        vol2.file_index = Some(2);

        let mut vol2_dup = base.clone();
        vol2_dup.rar_volume_idx = Some(2);
        vol2_dup.file_index = Some(3);

        let mut dsu = VetoDsu::new(vec![base, vol2, vol2_dup]);

        assert_eq!(dsu.union(0, 1, IntraReleaseGrouping::can_group), MergeResult::Merged);
        assert_eq!(
            dsu.union(0, 2, IntraReleaseGrouping::can_group),
            MergeResult::Vetoed(IntraReleaseVeto::DuplicateVolumeIndex),
            "non-root member volume 2 must veto duplicate volume 2"
        );
    }

    #[test]
    fn rar5_volume_number_parses_archive_flags_and_optional_common_fields() {
        // Volume 1: common header_flags = 0x01 (extra area present, size=4), archive_flags = 0x01 (volume attribute, no vol num)
        let mut vol1 = b"Rar!\x1A\x07\x01\x00".to_vec();
        vol1.extend([0u8; 4]); // CRC32
        vol1.extend([8, 1, 0x01, 4, 0x01]); // size=8, type=1 (Main), common_flags=0x01, extra_size=4, archive_flags=0x01
        assert_eq!(Par2Bridge::parse_rar5_volume_num(&vol1), Some(1));

        // Volume 3 (encoded as 0-based 2): common header_flags = 0x01 (extra_size=2), archive_flags = 0x03 (volume + vol_num present), vol_num = 2
        let mut vol3 = b"Rar!\x1A\x07\x01\x00".to_vec();
        vol3.extend([0u8; 4]); // CRC32
        vol3.extend([9, 1, 0x01, 2, 0x03, 2]);
        assert_eq!(Par2Bridge::parse_rar5_volume_num(&vol3), Some(3));
    }

    #[test]
    fn lcm_compatibility_is_symmetric_across_argument_order() {
        let a = ReleaseCandidate {
            name: "rel_a".into(),
            full_md5: None,
            size: 100_000_000,
            unpacked_crc32: None,
            yenc_total: Some(100),
            creation_salt: None,
            segment_uid: None,
            lcm_matched_intervals: 0,
            yenc_part_size: None,
        };
        let mut b = a.clone();
        b.name = "rel_b".into();
        b.yenc_total = Some(200);
        b.lcm_matched_intervals = 4;

        assert!(a.is_lcm_compatible(&b));
        assert!(b.is_lcm_compatible(&a));
        assert_eq!(
            InterReleaseDeduplication::can_deduplicate_as_twin(&a, &b),
            InterReleaseDeduplication::can_deduplicate_as_twin(&b, &a)
        );
        assert_eq!(InterReleaseDeduplication::partition_releases(&a, &b), ReleasePartition::Twin);
        assert_eq!(InterReleaseDeduplication::partition_releases(&b, &a), ReleasePartition::Twin);
    }

    #[test]
    fn ambiguous_16k_prefix_uses_rar5_volume_or_remains_unresolved() {
        let make_desc = |file_id: [u8; 16], full_md5: [u8; 16], md5_16k: [u8; 16], size: u64, name: &str| {
            let mut body = Vec::new();
            body.extend_from_slice(&file_id);
            body.extend_from_slice(&full_md5);
            body.extend_from_slice(&md5_16k);
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            while !body.len().is_multiple_of(4) {
                body.push(0);
            }
            let mut pkt = Vec::new();
            pkt.extend_from_slice(b"PAR2\0PKT");
            pkt.extend_from_slice(&((64 + body.len()) as u64).to_le_bytes());
            pkt.extend_from_slice(&[0u8; 16]);
            pkt.extend_from_slice(&[0x77; 16]);
            pkt.extend_from_slice(b"PAR 2.0\0FileDesc");
            pkt.extend_from_slice(&body);
            pkt
        };

        // Two RAR5 volumes of equal size (50_000) that share the exact same 16 KiB prefix
        // Wait: if they share the first 16 KiB, they have the same md5_16k.
        // For non-RAR5 files with identical first 16 KiB and no CRC, neither should be resolved
        // rather than assigning the wrong filename/full_md5 based on candidate order.
        let shared_head = vec![0x42u8; 16_384];
        let shared_16k: [u8; 16] = md5::Md5::digest(&shared_head).into();
        let par2_non_rar = [
            make_desc([1; 16], [0xA1; 16], shared_16k, 50_000, "disc1.iso"),
            make_desc([2; 16], [0xB2; 16], shared_16k, 50_000, "disc2.iso"),
        ]
        .concat();

        let base_run = CandidateFileRun {
            filename: "obf_a.bin".into(),
            group: "g".into(),
            poster: "p".into(),
            size: 50_000,
            yenc_total: None,
            yenc_part_size: None,
            file_index: Some(0),
            rar_volume_idx: None,
            archive_format: None,
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: true,
        };
        let mut run2 = base_run.clone();
        run2.filename = "obf_b.bin".into();
        run2.file_index = Some(1);

        let mut runs = vec![base_run, run2];
        let heads = vec![
            ArticleHead { file_index: 0, payload: shared_head.clone() },
            ArticleHead { file_index: 1, payload: shared_head },
        ];
        let res = Par2Bridge::resolve(&par2_non_rar, &mut runs, &heads);
        assert_eq!(res.resolved_count, 0, "ambiguous 16 KiB prefix without disambiguator must stay unresolved");
        assert!(!res.success);
        assert!(runs[0].full_md5.is_none());
        assert!(runs[1].full_md5.is_none());
    }
}
