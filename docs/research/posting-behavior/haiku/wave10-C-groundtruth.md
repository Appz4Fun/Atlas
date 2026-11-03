# Wave 10-C: ground-truth sources and evaluation design for post-linking

Scope: public sources that could label which Usenet posts belong to one release, plus an evaluation design. Time budget was about 10 minutes and about 25 tool calls. Tags: **verified** (fetched page confirms it), **docs** (vendor or spec text, read via a fetch summary), **forum claim** (user post, not authoritative), **guess** (my inference, not checked).

Caveat up front: fetches were summarised by a small model. Quotes are the summariser's rendering. Re-check any URL before relying on it.

## 1. Public NZB sources

### Anime Tosho (animetosho.org) - most useful, but shut down for new content

- **Shutdown notice (verified).** https://animetosho.org/about/shutdown2 says new torrent ingestion halted around 2026-05-09 GMT, updates ceased around 2026-05-10, and the updates-server lease was expected to end around 2026-05-16. The feed/storage server was "set to expire in October" 2026. The website is planned for termination in May 2027. Today is 2026-11-02, so the live feed and API are probably gone. The notice does not say explicitly whether NZB downloads or the API stop. Treat that as an open question to verify.
- **Data dump (verified).** https://animetosho.org/about/data lists:
  - `https://animetosho.org/at-data.torrent` (full data torrent, about 1.01 TB), and
  - `https://animetosho.org/at-dbdump.nzb` (database dump only).
  - The data can be used "in any way you like." No licence is named. The uploader seeds only for "a few days."
- **Layout (verified).** The dump has a MariaDB SQL dump (`database-dump.7z`, with `timebase.txt` holding the last-ingest timestamp). Storage folders are `nzb/` (gzipped NZBs made when AT uploaded to Usenet, keyed by AT torrent ID), `torrents/` (keyed by BTIH), plus attachments, sframes and sshots. Files join to database tables such as `toto_files`.
- **Not in the dump, per the page (verified).** No CRC export is mentioned, and no Usenet Message-ID export is mentioned. Info hashes are present (`toto_toto.btih`).
- **Feed (verified via forum).** SABnzbd forum thread https://forums.sabnzbd.org/viewtopic.php?p=120055 reports an undocumented `only_nzb=1` option on the RSS feed. Forum claim only.
- **Labels Anime Tosho gives.**
  - (c) alts: one AT torrent ID can map to one NZB. Several Usenet postings of the same AT torrent would be alts. Grouping by AT ID is a labelled "same release" key. Guess: AT uploads each release once, so this is a partial, uploader-side label, not a full label.
  - Infohash grouping (torrent side) can link releases across Nyaa or other mirrors. Guess on how much that overlaps with Usenet posts.
  - Gaps: AT labels one upload, not "all Usenet posts of that release across groups." Split posts across posters and groups are probably not represented as such. Guess.
- **Terms.** No ToS or licence beyond the "any way you like" line (verified). Anime content is mostly fine to use for research, but check local law.

### NZBIndex (nzbindex.com)

- **robots.txt (verified).** https://nzbindex.com/robots.txt blocks AI training crawlers outright (ClaudeBot, GPTBot, CCBot, anthropic-ai, and others). `User-agent: *` is allowed except `/api/`, with `request-rate` 1 per 3600 s and a visit window 02:00 to 06:00. Content-Signal: `search=yes, ai-train=no, use=reference`.
- **Implication.** Do not use NZBIndex for training data. Using it for evaluation labels is a separate question, but it is a reserved-rights site and bulk fetching would break the robots rate limit.
- **API / bulk NZB download.** Not verified. The disallowed `/api/` path suggests an API exists but is not for crawlers. Guess.
- **Labels.** Each NZB result has a file list and poster and group metadata. This is a good source for (a) split-release labels in a sample, because NZBIndex often groups by subject. Tag: guess until checked.

### BinSearch (binsearch.info)

- **robots.txt.** Fetch returned no usable content. Not verified.
- **Bulk / ToS.** Not verified. Keep it out of the bulk plan until checked.

### nzbking.com

- Not checked in this wave.

### Spotnet / Spotweb (free.pt)

- **Structure (verified via https://bafybeiemxf5abjwjbikoz4mc3a3dla6ual3jsgpdr4cjr3oz3evfyavhwq.ipfs.4everland.io/wiki/Spotnet.html, a mirror of the Spotnet wiki).**
  - Spots are XML in the article header, RSA-signed, carrying NFO-like metadata.
  - The NZB is a zipped file elsewhere on Usenet, referenced from the spot. NZB group `alt.binaries.ftd`. Header group `free.pt`. Comment group `free.usenet`. Report group `free.willey`.
  - A spot's Message-ID is itself a shareable pointer to the full post.
- **Labels.** A spot defines one release and points to its NZB. That gives a release label. It does not give per-segment group or poster information outside the NZB. Guess: NZBs carry segment Message-IDs and groups, but this is not verified here.
- **Terms.** Not verified. Spotnet is a public free network and reading it does not need a licence. Check the rules of any Spotweb instance you use.

## 2. Can these give labels?

| Label | Source | Verdict |
|---|---|---|
| (a) split posts across groups/posters = one release | AT NZB: one AT upload, which is one release by definition, but segments may span groups | Partial. Guess. |
| | NZBIndex / Spotnet NZB: a spot or NZB is one release, so its file set is the label | Likely good. Guess. Needs an NZB parse to confirm segment groups. |
| (b) twins (same NZB reposted later) | AT: multiple AT torrent IDs with the same BTIH or same file set | Possible. Guess. |
| | Spotnet: same release spotted twice | Possible. Guess. |
| (c) alts (same release, different container) | AT: same content, different AT torrent ID, or same BTIH across mirrors (Nyaa, nekoBT, Onalrie) | Best candidate. AT dump has torrents keyed by BTIH and `nyaasi_archive` / `anidex_archive` / `nekobt_archive`. Verified that these folders exist. |

Key limitation: the dump has no Usenet Message-IDs (verified: not mentioned). To use AT as ground truth for our linker, we need to parse the `nzb/` gzips and read the `<segment>` Message-IDs and `<group>` tags inside each NZB. That is the only way to join AT labels to actual posts. Guess that NZB format carries these (NZB spec, docs, not re-fetched here).

## 3. Prior evaluation work

- **Record linkage with group structure (verified).** https://www.ncbi.nlm.nih.gov/pmc/articles/PMC8142966/ proposes a record-level cluster evaluation for group-based linkage. It assigns each ground-truth record to one of seven categories (SS, SG, GS, GG_E, GG_M, GG_m, GG_W), using Jaccard matching of clusters, with AUC over thresholds. It argues pairwise P and R are ambiguous for clusters (its example gets P=0.6, R=0.667 for three different clusterings). Use this as a secondary metric.
- **Linkage estimation (verified).** https://ijpds.org/article/download/3102/6395 uses public datasets with known true links to check unsupervised precision, recall and F estimates, and notes that they track ground truth within about 7% for the best methods. Relevant to the idea of estimating precision when labels are scarce.
- **Usenet-specific evaluation.** None found. The search for an academic Usenet corpus or NZB dump on archive.org returned only generic NZB explainers and tools (forum and wiki pages). No paper or dataset measuring indexer matching precision or recall was found. Treat this as an empty result, not proof of absence.

## 4. Design

### Metrics
- Pairwise precision, recall and F1 over post pairs (primary, easy to explain).
- B-cubed precision and recall per post (robust to cluster size; add it, the 2020-era record-linkage literature uses it, not verified here).
- Cluster purity and inverse purity (secondary, interpretable).
- The seven-category record-level measure from PMC8142966 (diagnostic: which failure mode dominates, SG vs GS vs GG_W).
- Report each per label type (a, b, c) separately, since they are different failure modes.

### Sampling plan
- Pick 3 to 5 groups from the large binary groups (e.g. one anime, one movie or TV, one software or ebook, one music). Spread across hierarchy levels.
- Use 7 to 14 consecutive days for the sample, not one day, so twins and reposts have a chance to show up.
- Stratify by number of posts per release (1, 2-5, 6+) so singletons are not overrepresented.

### Dev/test split
- Split by release, not by post. Keep all posts of one release in the same split, otherwise leakage inflates recall.
- Split by time as well (dev on earlier days, test on later days) to see drift.

### Cheap hand labelling
- Show an annotator the subject, poster, group, date, file name and size of two posts. Ask "same release? yes / no / unsure". Two annotators on 10% for agreement.
- Use the AT-derived pairs (where the NZB parse works) as a silver set for (c). Hand-label only the disagreements and the top-scoring false positives.
- Budget: about 300 pairs per label type for dev, 300 for test. Enough for a rough precision estimate, not for fine differences.

## Recommended evaluation plan

- Parse AT `at-dbdump.nzb` / the `nzb/` gzips into release-level labels with segment Message-IDs and groups. Verify the NZB format assumption on 20 samples before using it as truth.
- Use the AT torrent ID and BTIH as the (c) labels; treat "same BTIH, different AT ID" as alts and "same file set, different AT ID" as twins.
- Build a sample of 7 to 14 days across 3 to 5 groups, stratified by posts per release, and split dev and test by release and time.
- Score pairwise P/R/F1, B-cubed P/R and the PMC8142966 categories per label type, and report them separately for (a), (b) and (c).
- Hand-label about 300 pairs per split with two annotators on 10%. Use AT pairs as silver labels and check against the hand labels.

## Open items
- Confirm whether AT NZB downloads and the `only_nzb` feed still work after the October feed expiry (not verified).
- Check NZBIndex and BinSearch API and ToS directly before any bulk use (not verified; NZBIndex robots.txt disallows `/api/`).
- Confirm the NZB spec on segment and group tags (docs, not re-fetched).
