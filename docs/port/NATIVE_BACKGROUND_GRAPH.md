# Native background graph composition

The background entrypoint has one checked PostgreSQL runtime. The old SQLite
bootstrap/conversion, shard compaction timer, arbitrary StageWork services,
shard checkpointer, sealed offload, mirrored catalog and legacy worker branches
are removed. `run_until` selects the same checked native startup. Existing
native provider admission, original Owner/session, cursor preflight, overdue
monitor, outage classification, real drain and word Keeper remain.

The accepted daily media-count service now produces the actual native
`backfill_media` rows on its original owner lane. The existing group-priority
reader runs natively and a committed completed count refreshes its selection
immediately. The producer keeps the old row-count/4-GiB/title classification and
24-hour cadence; incomplete scans never replace the previous tally. Each page
releases the owner before yielding. Its five-second work bound excludes the
inherited Owner mutex/preflight wait; original overdue quarantine and drain
remain authoritative.

The accepted catalog Observer is retained for the attempt, uses its original
Owner and catalog identity, and is really joined before owner close. Private
observer cancellation does not stop the Owner. Publication and the Stats callback
happen outside its owner lock. `stats.json` database size is unknown (`null`)
until a completed native relation-size sample, rather than a SQLite-file total
or a fabricated zero. Existing stage-owned catalog total metrics remain separate.
An observer failure preserves its cause and original failure classification.

Six scheduler controls retain all their assertions against fresh migrated real
PostgreSQL fixtures: due sweep precedence, configured ban, busy-home sweep,
one chunk worker ahead of groups, connection-scaled chunk shares, and lone-worker
sweep/group alternation. Existing native cursor preflight/queued-stop/Fatal tests
remain; their removed compaction argument disappears with that legacy branch.

All builds, PostgreSQL/provider work and tests are **UNRUN**. Rustfmt parsing and
whitespace checks are source checks only. Overview service glue, retained grouping
fixture ports and other source components still need composition before the full
graph build. No service activation, deployment, data deletion or full SQLite-free
qualification is claimed by this component.
