# Public synthetic TLS fixtures

Every certificate and private key in this directory is PUBLIC, test-only data.
They were generated solely to exercise sealed-log TLS and authorization source
regressions. Never use any of these identities, keys or CAs in an actual service.
No operator certificate, credential, configuration or protected endpoint was read.

The test CA signs localhost server, authorized client, and ungranted client
identities. A separate untrusted test CA signs the rejected client. Issuer private
keys were transient source-generation material and are not needed by the tests.
Certificates use P-256/ECDSA-SHA256 with explicit serverAuth or clientAuth purposes.
Validity covers the fixture generation date in November 2026 through 2046. Tests
use rustls verification against these explicit roots, never an accept-all verifier.
The cert/key bytes are source fixtures, not evidence that any TLS test has run.
