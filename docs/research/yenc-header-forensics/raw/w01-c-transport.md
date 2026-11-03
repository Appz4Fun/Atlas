# Wave 1 / Agent C: NNTP transport headers (Haiku)
- Path (RFC 5536 3.1.5): the rightmost entries show the injector. Group by injecting server. HEAD only. (2/5)
- Injection-Info (3.2.8): posting-host, posting-account, logging-data, mail-complaints-to. The same source SHOULD share posting-account, which makes it the account-level grouping key. HEAD only. (3/5)
- Injection-Date (3.2.7): injection time, separate from Date; clusters per connection. HEAD. (2/5)
- Xref: server group:num per group; one Xref listing several groups = a true crosspost (one article). Overview only via Xref:full. (3/5)
- NNTP-Posting-Host / X-Trace (obsolete): some hosts hash it unsalted, which can be precomputed. HEAD. (2/5)
- Date-Received: obsolete, ignore.
- Whether providers strip or rewrite headers was NOT verified.
Leads: RFC 3977 8.4 default OVERVIEW.FMT; real backbone header dumps; big-8.org/wiki/Usenet_headers.
