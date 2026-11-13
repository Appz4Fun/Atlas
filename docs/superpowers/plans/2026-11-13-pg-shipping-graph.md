# PostgreSQL Shipping Graph Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Source changes are split among existing owners; actual compilation and tests remain with the coordinated validation owner.

**Goal:** Remove SQLite from the shipping source and test graph while retaining native PostgreSQL behavior, valuable-cache preservation, and pure parsers/codecs/NXV.

**Architecture:** Keep the existing native owners, readers, workers, and closed mutations. Extract pure facts without changing hashes or identities; remove legacy halves only when their callers use real native successors. Retain the exact result-v1 byte stream and ownership/error contracts rather than adding another generic storage framework.

**Tech Stack:** Rust, PostgreSQL, NXV, existing native Step and cache actors.

**Spec:** Fresh PostgreSQL cutover directive, `AGENTS.md`, and `docs/POSTGRES_FRONTDOORS.md`.

## Global Constraints

- Base is the reviewed native ports/dependency checkpoint plus the accepted PostgreSQL product frontdoor.
- Source-only work: no Cargo builds/tests/lock changes, database/config-secret reads, providers, services, data-file deletion, or public changes.
- Preserve ownership, outage causes, drain/stop, lease/claim/ack, digest receipts, cache quotas and enabled-worker contracts.
- Preserve pure parsers, archive/member association, native backup/restore, footer/README attribution, NXV durability and provider ordering.
- Never hide SQLite behind optional, test-only, or development features to claim closure.
- Preserve the original/offline valuable-data sources; source retirement does not prove cache import parity.

## Review Focus

- Typed Step results retain target/Observed-participant validation and exactly the same v1 tag/value bytes.
- Native inventory/cache failures retain their PostgreSQL error chain and cannot become misses or retry acknowledgements.
- Removing a legacy scheduling branch retains every supported native worker and its stop/reload semantics.
- Pure extraction preserves identity, bitsets, hash inputs and singleton atomic state.
- A complete graph claim requires every supported target/feature and actual PostgreSQL/no-fallback tests; refusal tests are not maintenance parity.

## Work Packages

- [ ] Make replay native-only: remove its SQLite writer, shard counts, checkpointer and Prepared compatibility; retain destination checks, full-block archive behavior and native report semantics.
- [ ] Close native StepData/StepCtx/StepError/StepWrite: remove SQLite inventory wrappers, raw SQL/shadows and callbacks; preserve native result-v1 encoding and map each retired SQLite assertion to a native or pure successor.
- [ ] Extract pure catalog facts from db/store: keep exact types/functions and atomic statics, rewire native consumers, then remove remaining SQLite engine call graph after their consumers close.
- [ ] Remove legacy Web/API/TUI/search/statistics halves in closed caller groups; retain actual native routes, auth, assets, footer, pure presentation/math and useful telemetry.
- [ ] Integrate the independently reviewed history/import/converter retirement, retaining native external-cache backup and the native history gate.
- [ ] Make indexer/shared SQL native-only, retaining owner sessions, queued native save/cursor/NXV behavior and cancellation-safe cleanup.
- [ ] Remove feature/raw-cache/media/PreDB/archive/Spotnet legacy halves only after native Step and worker interfaces are composed.
- [ ] Port retained behavior tests to real PostgreSQL/pure vectors; retire only strict legacy conversion/storage targets with an explicit coverage inventory.
- [ ] Remove the root SQLite dependency after source closure; one coordinated lock writer resolves the lock, followed by clean normal BIN/LIB/all-target/feature validation and actual native runtime qualification.

Overview repair, its pure whole-post resolver, and native service glue have an independent owner and remain outside other mutation allowlists. Each checkpoint is signed and reviewed before integration. All new or ported tests remain UNRUN until coordinated validation executes them.
