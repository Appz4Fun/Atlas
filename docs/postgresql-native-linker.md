# Native PostgreSQL linker

The native ingestion profile stages NXV headers in PostgreSQL before release inference. Each work epoch freezes its clock, source generations and published manifest prefixes. Durable keyset reducers perform deduplication, grouping and ordering; the process retains a fixed inventory page, one verified source block and a bounded publication page. Each reducer checkpoints its subphase and input position with a bounded page of row effects. A large epoch, upload or membership record does not become one transaction. A single large poster, file or upload does not require a complete in-memory article tree.

All state-changing linker transactions force local synchronous commit. Raw expiry is pinned while source inventory and staging are incomplete. Afterwards, expiry requires durable coverage for the exact generation and captured prefix; an appended suffix or recreated segment cannot inherit coverage. Database errors or unknown coverage retain the raw source. The background owner's existing PostgreSQL session performs both linker operations and admitted release writes.

Native startup bypasses the legacy in-memory metadata loader. It retains no whole-arena mapping: the current snapshot pins one file descriptor and maps one format block at a time, including during recovery. Completed block mappings are dropped; memory safety does not depend on advisory page eviction. Source bodies are verified once while staging a block. Row pages resume from the format's 128-row checkpoints. A restart verifies the incomplete block again; already complete sources are recognized by immutable generation and exact manifest bytes without re-reading their bodies. Active chain matching and inventory traversal use explicit indexed continuations, including progress through empty groups. A registry of currently uncommitted slabs captures flush barriers without rescanning historical groups. A reopened Store recaptures any incomplete barrier before inventory, while preserving the frozen epoch clock. Retention uses a durable contiguous coverage watermark; gap repair advances at most 32 indexed block probes per call.

Publication plans preserve release names, home groups, file and part numbering, leads, metadata generations and logical time. A writer commit followed by a lost receipt replays that same plan. Final metadata acknowledgement occurs only after all publication receipts, in child-first pages hidden behind the acknowledgement phase until its final completion marker. Exact part and file ordering use restartable two-head merge runs with integer scan keys, so arbitrary message IDs and posters remain payloads instead of oversized B-tree entries. Poster equality handles retain exact-byte collision checks. A resumed epoch finishes before startup expiry removes old placement or upload metadata.

## Historical state reconciliation

Legacy block counts do not identify a segment generation and do not prove that every article was published. During one-time import, those counts preserve old-before-late duplicate priority. Previously unpublished eligible exact IDs still count as new material, but only when their hours are ready. The decision and captured generation are durable. Import scans persist byte offsets and committed line ranges. Large F/U memberships use validated shadow records and paged replacement; historical linked-hour ranges also checkpoint between pages. Initial metadata expiry uses the same bounded child-first procedure as subsequent startup expiry. Legacy hash-only identities remain conservative compatibility membership; new identities retain their complete canonical bytes beside a SHA-256 key. A digest collision is an explicit error, not a merge.

## Retained input and disk capacity

Completed epoch bodies are removed in bounded child-first pages. Exact duplicate source copies are compacted only when a durable full published identity proves their disposition. Unresolved sub-threshold, delayed, future, expired-placement or otherwise ambiguous rows remain accessible in PostgreSQL. There is no speculative age-based deletion of this input. An epoch records a scalar source high-watermark rather than copying the whole pending inbox each time.

Database storage can therefore grow with unique retained input, just as the release catalog does. A storage outage can also grow NXV because retention fails closed. Before production use, the operator must verify database and NXV free-space headroom against measured growth, configure a disk-capacity fence, and verify that the resource supervisor stops intake before that headroom is exhausted. A passing memory test is not evidence of sufficient disk capacity. Automatic disposition of unresolved input is a separate policy change.

Use catalog estimates and relation sizes for regular monitoring, rather than repeatedly counting all stored rows. For example, in the configured application schema:

```sql
SELECT relname,
       greatest(reltuples, 0)::bigint AS estimated_rows,
       pg_total_relation_size(oid) AS bytes_including_indexes
FROM pg_class
WHERE relnamespace = current_schema()::regnamespace
  AND relname IN ('linker_staged_rows', 'linker_source_blocks',
                  'linker_epochs', 'linker_duplicate_work')
ORDER BY relname
LIMIT 4;
```

The row estimate depends on PostgreSQL statistics and includes any in-flight published rows awaiting cleanup. Relation sizes include indexes and TOAST. Inspect retained payloads with keyset pages, for example `ORDER BY source, ordinal LIMIT 256` with a `(source, ordinal)` continuation; do not reconstruct the entire inbox in an operator process.

Native archive lead traversal is deliberately unavailable in this slice. The native ingestion profile continues to reject postprocessing and inline BODY work until a separately reviewed durable probe-window implementation is ready. Legacy archive behavior is unchanged.
