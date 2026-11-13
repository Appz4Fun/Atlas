# Pinned tiny_http source

This directory contains the crates.io `tiny_http` 0.12.0 archive, from
`https://crates.io/api/v1/crates/tiny_http/0.12.0/download`.
Its SHA-256 is
`389915df6413a2e74fb181895f933386023c71110878cd0825588928e64cdc82`.
The archive records upstream Git revision
`212b1c45852fef2093dc1374875a9393c55eb4b9`.

`UPSTREAM-SHA256.json` records every original archive file hash. The original
MIT and Apache 2.0 licenses, package metadata, examples and tests are retained.
These upstream files differ:

- `src/util/equal_reader.rs`: reuse an 8 KiB stack buffer while discarding an
  unread fixed-length body, instead of allocating its declared remainder.
- `src/request.rs`: add `Request::respond_raw`, a closure API that retains
  the request through raw response writing and flushing, releases the ordered
  writer, then performs the same response notification as `respond`.
- `src/lib.rs`, `src/client.rs`, `src/connection.rs` and
  `src/util/refined_tcp_stream.rs`: add an optional HTTP-only bounded
  constructor, connection admission, incoming protocol limits and transport
  read/write/absolute deadlines. The new `src/bounded.rs` holds their state.
  Bounded body wire admission wraps the raw source before EqualReader or the
  chunk decoder, including framing/extensions/trailers in its positive byte
  allowance. Overflow returns an error and closes, rather than returning EOF.
  Ordinary constructors keep their existing behavior.
- `src/response.rs`: let bounded Request cleanup privately emit
  `Connection: close` when it cannot safely retain a connection.
- `src/util/messages_queue.rs`: optional queue close/drain on bounded server
  shutdown, releasing queued requests outside the queue lock. Bounded shutdown
  closes admitted transports first and rejects further queue admission.

Bounded Request cleanup tracks transport body completion, closes incomplete
bodies before destruction and notifies the connection loop after cleanup.
Raw writer extraction retains the request in bounded mode. Bounded HTTP
pipelining is processed sequentially with safe keepalive after complete bodies.

Atlas uses the closure API for completed NZB spools with bounded header output.
The original public APIs are unchanged. The new API preserves raw I/O errors so
Atlas can observe client cancellation.

The fixed buffer alone bounds allocation. Applications must opt into the
bounded constructor for request/transport deadlines. The application must
also propagate cancellation/deadlines into its generation work; retaining a
deadline handle retains connection admission.

All Atlas changes are source preparation pending regression and resource
qualification; vendoring does not authorize service startup.
