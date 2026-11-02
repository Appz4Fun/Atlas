# Wave 2 / Agent A: provider-added headers (Haiku). Low confidence.
- X-Received-Body-CRC / X-Received-Bytes: no primary source defines them (searches over sabnzbd, reddit, Omicron/Highwinds found nothing). Their semantics are UNVERIFIED. A live HEAD test on two known byte-identical reposts is the only way to tell if they are a HEAD-only twin key. X-Received-Bytes is probably per-ingest, so not a content key.
- X-Trace: INN workers 2000 says the injecting site adds it and transit sites change only Path/Xref. No syntax confirmed. https://lists.isc.org/pipermail/inn-workers/2000-July/003376.html
- Injection-Info/Injection-Date/Xref/Organization: no hashes. posting-account SHOULD be private (an opaque per-account token). RFC 5536.
- NNTP-Posting-Host: obsolete (RFC 2980), replaced by the Injection-Info posting-host param.
- Bottom line: no verified HEAD-only twin key. Account linking only if the server exposes posting-account.
Leads: grep the SABnzbd/NZBGet/nzbhydra2 source for "X-Received"; real alt.binaries header dumps; Highwinds FAQ.
