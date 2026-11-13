# sealed-log

An append-only SLG1 store with durable group commit and exact 21-byte locators.
Data operations and Prometheus scrapes require mutually authenticated TLS.

Configure a server certificate chain/key, a client CA, and explicit client
leaf-certificate SHA256 grants. For example, using your own certificate files:

```sh
sealed-log /data --cert /certs/server.pem --key /certs/server-key.pem \
  --client-ca /certs/client-ca.pem --grant CERTIFICATE_SHA256:READ,APPEND,STATS
```

Replace `CERTIFICATE_SHA256` with 64 hex digits from the client's DER certificate,
without colons. A CA-verified certificate without a grant has no access. Grant
operations separately with comma-separated `READ`, `APPEND`, and `STATS`; the
metrics endpoint also requires `STATS`. Duplicate identities/operations and unknown
operations are rejected. Client keys and server keys remain private operator files;
only certificate fingerprints appear in grant arguments.

Data defaults to `127.0.0.1:7711`; metrics defaults to `127.0.0.1:7712`. Use
`--listen HOST:PORT` and `--metrics HOST:PORT` for explicit binds, or an empty
`--metrics` to disable scrapes. The existing explicit `SEALED_LOG_METRICS` setting
is retained, with a CLI selection taking precedence. Metrics are HTTPS with mTLS,
not an anonymous HTTP bypass. The Docker entrypoint leaves the address unset so
these private defaults apply; the operator supplies certificate mounts and flags.

Library clients construct `ClientTls::from_pem_files(server_ca, client_cert,
client_key, expected_server_name)` and call `Client::connect(address, &tls)`.
Servers pass an `Arc<ServerTls>` to `serve` and `metrics::serve`. Server-name
verification and client-certificate verification are mandatory. There is no
plaintext fallback. Coordinate a compatible client/server release and certificate
configuration before deploying; this source change does not alter any running
container, endpoint or operator configuration. The existing inner operation/frame
payloads, SLG1 records and locator encoding are unchanged inside TLS.

Before spawning a peer, shared data/metrics admission allows at most eight active
connections by default. TLS handshake has a five-second absolute I/O deadline;
subsequent protocol requests share a 120-second absolute read/write deadline.
Metrics request I/O is capped at ten seconds and headers at 8 KiB. Library limits
may be configured within explicit validated bounds. These are I/O/admission bounds,
not a hard deadline for filesystem sync or a claim that authenticated 512 MiB
frames are a suitable working-set budget for every deployment.

A write, sync or rollover failure permanently fences that writer. Current,
queued and future appenders fail; direct callers retain the original I/O cause,
and the last successfully published tail is unchanged. Reopen validates the last
active segment with bounded scratch space. Only an entirely verified zero suffix
is unused reservation. Nonzero partial/malformed records or bad CRCs fail closed
without truncating or rewriting evidence. Legacy `tail` hints are neither trusted
nor advanced; they cannot certify a prefix or distinguish acknowledged data.
Older immutable records are CRC-checked when read; startup is not an archive-wide
health scan. Empty values and valid historical SLG1/locator bytes remain readable.

The synthetic TLS identities under `tests/fixtures` are PUBLIC test-only data.
Never deploy their keys or CAs. Use the dedicated integrity and TLS tests when qualifying a compatible release.
