# W08-C: Rust spec, Message-ID classifier and Highwinds session key

Spec only, no code. Atlas (Rust). Deps: `std` + `regex` only. Source: `scripts/classify.py` (first match wins) and `scripts/highwinds.py`. Corpus: `segs.tsv`, 72,334 rows, 246 NZBs; cols 0 nzb_id, 1 nzb_date (unix), 2 Message-ID, 3 MID host, 4 From.
Caveat: the w07-a summary "K" section is the trailing R field (highwinds.py `Kvals = x[4]` = group 4). Highwinds session K is the `$` field (group 3).

## 1. Tool labels

```rust
pub enum ToolLabel {
    NoAtSign, Nyuu, NyuuLoose, Private, NyuuStyleOtherHost, NgPost, GoPostStuff,
    NewsMangler, Postie, Nxg, JBinUp, PowerPost, Highwinds, Diablo, InnRefined,
    UsenetFarm, Giganews, GiganewsOther, YencBinPartN, Hex32TsHost, TsHostRandom,
    NntPoster, UsenetFarmLoose, Hex3Dollar7, AlnumRandHost, SyntheticA1, Unknown,
}
```
Zero corpus hits (kept, no linking claim until sampled): GoPostStuff, Postie, Nxg, JBinUp, InnRefined, Giganews, NntPoster.

## 2. Ordered rules

Use `Regex::is_match` on the full MID. ASCII classes only (`[0-9]`, not `\d`).

| # | Label | Regex | Extracted fields |
|---|---|---|---|
| 0 | NoAtSign | no `@` in MID | none |
| 1 | Nyuu | `^(?:[A-Z][a-z]){12}-[0-9]{13}@nyuu$` | nyuu_ms, prefix |
| 2 | NyuuLoose | `^[A-Za-z]{24}-[0-9]{13}@nyuu$` | nyuu_ms |
| 3 | Private | `-[0-9]{13}@PRiVATE$` | nyuu_ms |
| 4 | NyuuStyleOtherHost | `^[A-Za-z]{24}-[0-9]{13}@[^@]+$` | nyuu_ms, host |
| 5 | NgPost | `^[0-9a-f]{32}@ngPost$` | none |
| 6 | GoPostStuff | `^[0-9]+\$gps@gopoststuff$` | none |
| 7 | NewsMangler | `^[0-9]{10}\.[0-9]{5}\.[0-9]+@` | unix10, seq |
| 8 | Postie | `^[A-Za-z0-9]{32}@[A-Za-z0-9]{8}\.[A-Za-z0-9]{3}$` | none |
| 9 | Nxg | `^[0-9a-f]{40}@[0-9a-f]{21}\.[0-9a-f]{3}$` | none |
| 10 | JBinUp | `@JBinUp\.local$` | none |
| 11 | PowerPost | `@powerpost2000AA\.local$` | part N, total M via `^part([0-9]+)of([0-9]+)\.` |
| 12 | Highwinds | `^([0-9A-Za-z_%]{4,6})\.([0-9]+)\$([0-9A-Za-z_%]+)\.([0-9]+)@(fx[0-9]+\.[a-z]+[0-9]*)$` | T1 g1, N g2, K g3, R g4, host g5 |
| 13 | Diablo | `^[0-9a-f]{8}\$[0-9]+\$[0-9]+\$[0-9a-f]+(\$[0-9A-Za-z_]+)?@` | hex8 as unix, seq, pid, hosthash |
| 14 | InnRefined | `^[0-9a-f]+\$[0-9a-v]+\$[0-9a-v]+\$[0-9a-f]+@` | none |
| 15 | UsenetFarm | `^[A-Za-z]{32}@(UFK\|news\.usenet\.farm)$` | none |
| 16 | Giganews | `^[A-Za-z0-9_-]{32}@giganews\.com$` | none |
| 17 | GiganewsOther | `@giganews\.com$` | none |
| 18 | YencBinPartN | `^Part[0-9]+of[0-9]+\.[0-9A-F]{32}@[0-9]{10}\.local$` | part, total, hex32, ts10 |
| 19 | Hex32TsHost | `^[0-9a-f]{32}-[0-9]{10,13}@` | none |
| 20 | TsHostRandom | `^[A-Za-z0-9]{10,20}@[0-9]{10}\.[0-9]+[A-Za-z0-9]+$` | ts10 |
| 21 | NntPoster | `^[0-9a-f]{32}@` | none |
| 22 | UsenetFarmLoose | `^[A-Za-z]{32}@` | none |
| 23 | Hex3Dollar7 | `^[0-9a-f]{6,8}\$[0-9a-f]{6,8}\$[0-9a-f]{6,8}@[0-9a-f]{10}\.[0-9a-f]{5}$` | none |
| 24 | AlnumRandHost | `^[A-Za-z0-9]{20,28}@[A-Za-z0-9]{5,13}\.[A-Za-z0-9]{3}$` | none |
| 25 | SyntheticA1 | `^[a-z][0-9]*-[0-9]+@` | none |
| 26 | Unknown | nothing above matched | none |

Fixture flag (not a label): `is_fixture_host` = host ends in `example`, `example.com`, `example.invalid`, `test`, `nntune.test`, `news.example*`, `nothereatall` or `nowhere.example`. About 442 rows. Exclude from live statistics.

## 3. Precedence and ties

- Table order; first match wins; one label and one `rule_no` per MID.
- Overlaps: a Nyuu-shaped MID with host `nyuu` is Nyuu or NyuuLoose by case pattern; with any other host it is Private or NyuuStyleOtherHost.
- A parse failure (T1 not 4-6 symbols, symbol outside alphabet, u64 overflow) keeps the label, sets fields NULL and `parse_error = 1`. Never relabel.
- Time checks (flag, never reject): `|nyuu_ms/1000 - nzb_date| <= 300` else `ts_skew` (observed median +1 s, max +93 s); decoded T1 within `nzb_date ± 3600` else `t1_skew` (observed -865 s to +47 s). Tunables, INFERRED: `t1_win_s = 3600`, `n_slack = 5000`.

## 4. Function signatures

```rust
use regex::Regex;
use std::sync::OnceLock;

pub const CLASSIFIER_VERSION: &str = "w08-c-1";

pub struct HighwindsId {
    pub t1_raw: String, pub t1_unix: u64, // decode_t1 + 876645500
    pub n: u64,      // front-end counter, per article
    pub k: String,   // session key, constant per post
    pub r: u64,      // random, per article
    pub host: String // e.g. "fx09.am4"
}

pub struct Fields {
    pub nyuu_ms: Option<u64>, pub hw: Option<HighwindsId>,
    pub diablo_unix: Option<u64>, pub part: Option<(u32, u32)>,
    pub part_hex: Option<String>, pub ts10: Option<u64>,
    pub fixture: bool, pub parse_error: bool,
}

/// First match wins. Returns label and 1-based rule number.
pub fn classify(mid: &str) -> (ToolLabel, u8);
/// Fields for an already-labeled MID.
pub fn extract(mid: &str, label: ToolLabel) -> Fields;
/// LE base-64, alphabet 0-9a-zA-Z, '_'=62, '%'=63; value + 876645500.
pub fn decode_t1(t1: &str) -> Option<u64>;
pub fn parse_highwinds(mid: &str) -> Option<HighwindsId>;
pub fn is_fixture_host(mid: &str) -> bool;
/// Session key (host, k).
pub fn session_key(hw: &HighwindsId) -> (&str, &str);
/// Same (host, k), |t1 diff| <= t1_win_s, n within session range +/- n_slack.
pub fn same_session(a: &HighwindsId, b: &HighwindsId, t1_win_s: u64, n_slack: u64) -> bool;
fn rules() -> &'static [(ToolLabel, Regex)]; // OnceLock, table order
```

T1 decode: `value = sum(idx(c_i) * 64^i)`, `i` from 0 (first char least significant), then `+ 876645500`. Checked u64.

## 5. Suggested SQL columns

```sql
CREATE TABLE mid_class (
  seg_id INTEGER PRIMARY KEY, nzb_id TEXT NOT NULL, nzb_date INTEGER NOT NULL,
  mid TEXT NOT NULL, mid_host TEXT, from_addr TEXT,
  label TEXT NOT NULL, rule_no SMALLINT NOT NULL,
  is_fixture BOOLEAN NOT NULL DEFAULT 0, parse_error BOOLEAN NOT NULL DEFAULT 0,
  nyuu_ms INTEGER, hw_t1 TEXT, hw_t1_unix INTEGER, hw_n INTEGER, hw_k TEXT,
  hw_r INTEGER, hw_host TEXT, diablo_unix INTEGER, diablo_pid TEXT, diablo_hosthash TEXT,
  part_no INTEGER, part_total INTEGER, part_hex TEXT, ts10 INTEGER,
  classifier_ver TEXT NOT NULL
);
CREATE INDEX mid_class_hw ON mid_class (hw_host, hw_k, hw_t1_unix);
CREATE INDEX mid_class_label ON mid_class (label, nzb_id);
```

## 6. Test vectors (segs.tsv column 3)

| # | MID | Expected label (rule) | Fields |
|---|---|---|---|
| 1 | `KyPhPeJsTfQtGlIfDlGlPtEr-1582821143379@nyuu` | Nyuu (1) | nyuu_ms 1582821143379; skew 0 s |
| 2 | `LvNzBcByRuQwRyOeMsQhLpTy-1658672492641@NNNNNNN` | NyuuStyleOtherHost (4) | nyuu_ms 1658672492641; host NNNNNNN |
| 3 | `TaRgNiYiMjDeEgMyUhJzKoPk-1550633075658@PRiVATE` | Private (3) | nyuu_ms 1550633075658 |
| 4 | `481e19965e71433eaa543be30a426ac3@ngPost` | NgPost (5) | none |
| 5 | `4f9a3e8a$0$14349$c3e8da3$33a0879d@news.astraweb.com` | Diablo (13) | unix 1335508618; seq 0; pid 14349; hosthash c3e8da3 |
| 6 | `EaPmB.546176$JP7.228032@fx09.am4` | Highwinds (12) | T1 EaPmB, t1_unix 1503379236; n 546176; k JP7; r 228032; host fx09.am4 |
| 7 | `FaPmB.546178$JP7.180477@fx09.am4` | Highwinds (12) | t1_unix 1503379237; n 546178; k JP7; r 180477; same_session with #6 = true |
| 8 | `Part370of1855.0E062F401B214C0D8D50C60646D49F3B@1392561150.local` | YencBinPartN (18) | part 370/1855; hex32 0E062F40...; ts10 1392561150 |
| 9 | `VNdHifYKSAtPRNYRoIhyQLghUETQLDaC@UFK` | UsenetFarm (15) | none |
| 10 | `zskUjWrhSDUOZvkJGThaXsnaBkHaLycFCsXfRW@hL_ArB.lfoRvh.mMOe.siBGJt` | Unknown (26) | none; parse_error 0 |

## 7. Linking rule per label

| Label | Link key | Proves |
|---|---|---|
| Nyuu, NyuuLoose | nyuu_ms within 300 s of nzb_date | Same tool, same run when times match. Not an uploader key. |
| Private | host PRiVATE + constant From per NZB | Same tool and uploader config. |
| NyuuStyleOtherHost | host string equal | Same uploader (imitator, not Nyuu). |
| NgPost | none | Same tool only. |
| NewsMangler | unix10 prefix, seq | Same tool; unix10 groups one run (INFERRED). |
| PowerPost | part N/M within a post | Parts of one file, so same post. |
| Highwinds | same_session: (host, k), T1 window, N range | Same upload session across groups and NZBs. K is unique per NZB in corpus (7 of 7). |
| Diablo | pid and hosthash equal | Same server process on that front end. Server key, not uploader. |
| YencBinPartN | ts10 host part | Same uploader run (INFERRED). hex32 is unique per part, not a file key. |
| UsenetFarm, UsenetFarmLoose | none (presence test; C25 open) | Same tool family only. |
| Hex3Dollar7, AlnumRandHost, SyntheticA1, TsHostRandom, Hex32TsHost, GiganewsOther | host string equal | Same uploader config (INFERRED). Sample first. |
| Zero-hit labels, Unknown, NoAtSign | none | No claim. |

Caveats: no NZB hand-checked; unknown shapes inferred from form; most rows are fixture hosts, so results cover these 246 NZBs, not live traffic.

## Wave 9 corrections (validated on 72,334 segments, raw/w09-a-spec-validation.md)
- Rule 10 JBinUp: use `^[A-Za-z0-9]{19,20}_[0-9]+o[0-9]+@JBinUp\.local$`.
- Add rule 10b JBinDown: `^[^@]+\.[0-9]+-[0-9]+@JBinDown\.local$` (0 corpus hits, from nzbmonkey.py:112).
- Rule 22 UsenetFarmLoose: only accept hosts `UFK` or `news.usenet.farm`.
- Add an `nzb_date_bad` flag (date in the future or before 2000) and skip time-window checks when it is set.
- Field checks: Nyuu ms within ±1 day of NZB date for 99.56% (all misses are one NZB with a 2033 date); Highwinds T1 869/869 within ±3600 s.
