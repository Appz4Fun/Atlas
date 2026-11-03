# Wave 4 / Agent C: POST-level behaviour and Path tokens (Haiku)
- Nyuu has no server-assigns option. --keep-message-id; --input-raw-posts implies keep; --post-method XREPLIC uses a locally random article number. (help-full.txt)
- RFC 5537 3.5: the injecting agent MUST add a Message-ID if absent and MUST NOT alter one. It SHOULD add a `!.POSTED` Path diagnostic, MUST add a tail, and only it adds Injection-Info. A posting agent may add Injection-Date, which the injector must keep. So server-assigned IDs are compliant, and they come from posters that send no Message-ID.
- Path example: `uni-berlin.de!<...>!.POSTED.agency.bbs.nz!not-for-mail`. The .POSTED.<fqdn> element names the injecting site. (mail.python.org python-list 2018-September/887447)
- eternal-september Injection-Info posting-account/host are salted and rotate every 24h, so they only work as a same-day key. (groups.google.com/g/eternal-september.support/c/Lmsjw8vHQIQ)
- GAPS: the 240 response carrying the ID, backbone body dedupe, and real Highwinds/Giganews/Eweka Path lines.
