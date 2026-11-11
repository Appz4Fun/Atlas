# Native PostgreSQL outage recovery

The native background indexer stays alive when PostgreSQL is unavailable. Its
status is `postgres_paused`; it starts no provider work while waiting. Retries
wait 1, 2, 4, 8, 16, then at most 30 seconds. TERM/INT interrupts that wait.

A lost owner is never reconnected. Atlas cancels the indexing attempt, drains
workers and blocking database work, closes the old owner, then validates a new
session before acquiring sole ownership. Database, role, schema ownership,
catalog identity, schema version and configured destination must still match.
These ordinary catalog checks do not cryptographically authenticate a cluster.
Configuration, authentication, ownership conflicts, storage errors and panics
remain fatal. Native recovery never opens SQLite.

NXV data and PostgreSQL checkpoints survive the pause. A blocked coverage
callback prevents unacknowledged NXV input from expiring while the database is
unavailable. Healthy coverage resumes normal expiry.

An owner operation with no result after 30 seconds quarantines its attempt.
A local forward can acknowledge TCP bytes while silently withholding database
replies: the synchronous driver cannot forcibly cancel every blocked query.
Atlas keeps that old owner quarantined until the path returns or closes; it
never starts a replacement while the old callback is unresolved. TERM/INT is
also limited by that drain. The external resource guard retains its termination
and escalation policy. Socket connection timeout defaults to five seconds per
address; explicit connection settings can change transport detection times.

## An existing SSH transport

Deploy `scripts/atlas_pg_tunnel.py` beside `scripts/atlas_resource_guard.py`.
Run the tunnel helper under the existing resource guard with a 256 MiB envelope,
a private stop receipt and the established storage/swap/read limits. The helper
samples its one direct SSH child every 0.5 seconds with a 128 MiB memory ceiling.
Memory or measurement failures retain a stop fence for operator review.

Resolve an existing SSH alias privately before configuring the helper: it
ignores SSH configuration files. Supply explicit host, user, port, identity file
and known-hosts file, plus the remote PostgreSQL address and a fresh Unix socket
path in an owned directory with mode 0700. Use `--help` for the arguments. Host
keys are strict and authentication is noninteractive. Proxies, multiplexing,
daemonization and local commands are disabled.

The helper retries recognized transport losses, including a disconnected remote
host. Server-alive probes run every five seconds with three failures allowed.
Unknown failures, authentication and host-key failures are fatal. Existing
socket paths are refused on startup. After its exact child exits, the helper
can remove only its captured socket inode, and refuses changed files or a live
listener. TERM/INT stops and reaps that child within the outer guard's grace
period. A remote host returning while the helper's machine remains running is
supported; boot autostart remains an operator policy.
