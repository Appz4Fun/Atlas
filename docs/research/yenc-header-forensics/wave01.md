# Wave 1 merged (06:45-06:47Z)
Agents finished in about 1 minute each with 10-15 calls, so they did not use the full 10 minutes. Wave 2 asks for deeper source reads.

Best leads (cheapest first):
1. Message-ID right-hand side plus local-part shape is in OVER for free. newsmangler = `<unixtime.5dp.part@host>` (orders parts and dates the session), ngPost = `@ngPost` or a custom signature with UUIDs, Nyuu = an uploader template with a fixed length.
2. Subject zero-pad width (`[01/12]` vs `[001/120]`) is a per-batch signature (newsmangler, likely others).
3. The =ybegin and =yend lines (first and last lines of the body) show total= present or absent, field order, CRC hex case and width, and crc32= on every part or only the last. This is an encoder fingerprint read from about 2 lines.
4. Escape-set differences (leading space/tab/dot) and line length (128 vs 256) need the first body lines.
5. HEAD only: Injection-Info posting-account (account-level key), Path injector, Injection-Date clustering, Xref multi-group = a true crosspost.
Dead ends: no decoder bug reports naming tools; Date-Received is obsolete.
