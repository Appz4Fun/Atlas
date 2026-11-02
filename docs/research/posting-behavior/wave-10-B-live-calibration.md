# Atlas live-calibration plan (wave 10B)

Goal: refit the wave 8-9 thresholds (join window, 2 s stitch tolerance, >8 posts/front-end split, m/u weights) on live headers, then promote the linker. `/metrics`, VictoriaMetrics and Grafana bind to LAN addresses only; the web UI is never exposed; no Subject, From, file name or Message-ID enters a metric label.

## 1. Fields to persist

Per article (one row per msgid; Xref in a child table):

| Field | Source | Why |
|---|---|---|
| provider, backend_id | Atlas config | Numbers from one fixed backend |
| group, article_num, xref pairs | XOVER | Ordering; Xref aligns group counters |
| msgid_shape (enum: nyuu, ngpost, postie, highwinds, diablo, giganews, random24, random32, other) | parse msgid | Tool family + server-key grammar |
| srv_t, srv_fe/host, srv_session (S or PID), srv_counter (N) | decoded msgid | Fragments, per-front-end concurrency |
| nyuu_ms | `d{13}` in Nyuu IDs | Time truth |
| bytes | `:bytes` | Article-size veto (not Lines) |
| yenc part, total, size, begin, pcrc32, name_hash | body, when fetched | File identity, total veto, CRC fold |
| from (raw), from_shape | From | Tool fingerprint; never exported |
| date_hdr, posting_date, injection_date | overview / HDR | Date is fingerprint-only (postie `full` randomizes it; Nyuu `--date` fixes it) |
| t_derived, t_source (posting/injection/nyuu_ms/rank) | computed | The join input, with its provenance |
| ingested_at, provider_skew_s | Atlas clock, DATE | Feed lag, skew |
| subject_class (clear, hashed, random) | subject parser | Obfuscation share, labels |

Linker tables: run_id, FileKey kind, art_size, total, open/closed, tool; edge (a, b, dt, dt_norm = dt/(W/rate), evidence mask, weight, veto, threshold_set); component (confidence, body_fetches, version, current_grouping_id for shadow diff).

## 2. Metrics (Prometheus names, low-cardinality labels)

Labels are closed enums (≤9 values each). No group, front-end, poster or upload labels; that detail stays in the DB. ~800 series total.

- `atlas_link_join_gap_seconds` histogram{tool, basis="labelled|chosen"}: signed t_start(B) − t_end(A). Fit on `labelled` successor pairs; `chosen` edges are window-truncated. Buckets −30,−10,−5,−2,−1,−0.5,−0.25,0,0.12,0.25,0.5,0.67,1,2,5,10,30.
- `atlas_link_join_gap_norm` histogram{tool}: dt_norm, to fit the −1.2·W/rate edge.
- `atlas_link_time_residual_seconds` histogram{t_source}: t_derived − nyuu_ms (rank-in-second error).
- `atlas_link_frontend_concurrent_uploads` histogram{provider}: per (front-end, minute), distinct open posts (Highwinds/Diablo IDs only). Buckets 1,2,4,8,16,32,64.
- `atlas_link_uploads_total`{tool, par2="yes|no", files="1|2|3+"}.
- `atlas_link_uploads_by_groups_total`{groups="1|2|3-4|5+"} (spray share).
- `atlas_link_files_total`{subject_class}.
- `atlas_link_body_fetches_total`{stage="par2|hash16k|last|split|crc", result="ok|430|451|timeout"}, plus `atlas_link_files_linked_total` for bodies per file.
- `atlas_link_edge_weight` histogram; `atlas_link_components_total`{confidence, mode}.
- `atlas_link_vetoes_total`{veto}.
- `atlas_link_late_run_lag_seconds` histogram{provider}: run arrival minus its window close (sets H).
- `atlas_link_reevaluations_total`{trigger="late_run|par2|hash16k|crc|veto|oversize|takedown|repost|sweep"}; `atlas_link_edges_retracted_total`{confidence}.
- `atlas_link_eval_pairs_total`{label_source, outcome="tp|fp|fn", mode}: pairwise scoring; `mode="current"` scores today's grouping on the same labels.
- `atlas_link_shadow_diff_total`{kind="same|merge|split|new"}: shadow vs current grouping.

## 3. Ground truth for live precision/recall

Labels live in `gt_label(run_id, upload_label, file_label, source)`, which the linker never reads. For labelled runs, subject and yEnc name are masked (stage 2 forced to `FileKey::Anon`), then pairs scored.

1. **clear_subject**: uploads whose subjects give `name (n/m)` and a common base name. Biased to non-obfuscating tools; report per `tool`.
2. **nzb_known**: external NZBs (file list, segment msgids), never Atlas's own output (circular).
3. **postie_par2**: postie PAR2 FileDesc real names map hashed runs to files. Covers obfuscated uploads.
4. **nyuu_ms**: millisecond Message-ID times; drop runs where it is constant (`--date`). Time truth for gap and residual histograms (wave 9: median 0.12 s, p90 0.67 s, 98% < 2 s).

Freeze a 20% holdout per source (hash of upload_label); fit only on the other 80%. Xref crossposts are trivial positives: use them only for counter alignment.

## 4. Rollout

**Phase 0 (week 1): instrument.** Run `nntp_probe.py` per provider (skew, HDR on NNTP-Posting-Date, NEWNEWS, msgid shapes). Ship fields and metrics, linker off. Keep calibration columns 30 days.

**Phase 1 (weeks 2-3): shadow.** The linker writes only `link_shadow_*`; UI keeps current grouping. Shadow fetches get their own connection slot and token bucket (≤1 body per file, ≤5% of the download budget).

**Phase 2 (week 4): fit thresholds** on the 80% split, as a new `threshold_set`:
- Join window: per tool, [0.5%, 99.5%] quantiles of labelled dt_norm (replacing the 1.2 factor), with 0.5 s slack. Widen only if precision stays ≥ .98.
- Stitch tolerance: sweep 0-5 s against labelled fragments; keep 2 s unless another value is ≥ 3 points better.
- Split threshold: from the front-end concurrency p95 and whole-post rate per bucket.
- m/u weights: EM on postie_par2 + nzb_known.
- Idle horizon H and front-end skew: late-run lag p99 per provider; srv_t − nyuu_ms per front-end (spec assumes ±4 s).
- Replay the stored stream with the candidate set (a 1 h-lag replay must equal batch, test case 8), then shadow 7 days beside the old set.

**Phase 3: promote**, one confidence level at a time (proven → size_matched; time_only stays a hint in the UI). Promote when all of these hold for 7 consecutive days on the holdout:
- pairwise precision ≥ .98 (proven ≥ .995), recall ≥ current grouping + 5 points on obfuscated labels (current grouping wins clear_subject trivially);
- 0 proven edges retracted; retractions of other edges < 1% per day;
- body fetches ≤ 1.0 per linked file (p95 per day), 451/430 < 10%;
- ≤ 1 wrong in 50 hand-checked shadow `merge` diffs.

Rollback: feature flag to current grouping; shadow keeps running.

## 5. Grafana panels

1. **Join gap quantiles by tool** (repeat at 0.5, 0.99)
   `histogram_quantile(0.9, sum by (le, tool) (rate(atlas_link_join_gap_seconds_bucket{basis="labelled"}[6h])))`
2. **Share of gaps within ±2 s** (Nyuu target ≥ .98; check exporter `le` formatting)
   `(sum by (tool)(rate(atlas_link_join_gap_seconds_bucket{basis="labelled",le="2"}[6h])) - sum by (tool)(rate(atlas_link_join_gap_seconds_bucket{basis="labelled",le="-2"}[6h]))) / sum by (tool)(rate(atlas_link_join_gap_seconds_count{basis="labelled"}[6h]))`
3. **Front-end concurrency p95 vs split line (8)**
   `histogram_quantile(0.95, sum by (le, provider)(rate(atlas_link_frontend_concurrent_uploads_bucket[1h])))`
4. **PAR2 presence (public reference .48; .77 at 3+ files), spray, obfuscation**
   `sum(increase(atlas_link_uploads_total{par2="yes",files="3+"}[1d])) / sum(increase(atlas_link_uploads_total{files="3+"}[1d]))`
   `1 - sum(increase(atlas_link_uploads_by_groups_total{groups="1"}[1d])) / sum(increase(atlas_link_uploads_by_groups_total[1d]))`
   `sum(increase(atlas_link_files_total{subject_class!="clear"}[1d])) / sum(increase(atlas_link_files_total[1d]))`
5. **Live precision / recall, shadow vs current**
   `sum by (label_source, mode)(increase(atlas_link_eval_pairs_total{outcome="tp"}[1d])) / sum by (label_source, mode)(increase(atlas_link_eval_pairs_total{outcome=~"tp|fp"}[1d]))`, and recall with `outcome=~"tp|fn"` in the denominator.
6. **Bodies per linked file by stage**
   `sum by (stage)(increase(atlas_link_body_fetches_total[1d])) / ignoring(stage) group_left sum(increase(atlas_link_files_linked_total[1d]))`
7. **Confidence mix and vetoes** (stacked)
   `sum by (confidence)(increase(atlas_link_components_total{mode="shadow"}[1d]))`; `sum by (veto)(rate(atlas_link_vetoes_total[1h]))`
8. **Churn and shadow diff**
   `sum by (trigger)(rate(atlas_link_reevaluations_total[1h]))`; `sum(increase(atlas_link_edges_retracted_total{confidence="proven"}[1d]))` (alert if > 0); `sum by (kind)(increase(atlas_link_shadow_diff_total[1d]))`
