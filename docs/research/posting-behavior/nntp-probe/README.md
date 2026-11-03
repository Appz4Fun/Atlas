# NNTP capability probe: checklist

`nntp_probe.py` is a read-only probe. It needs only Python 3 and the standard library; it does not use nntplib, which was removed in 3.13. Run it yourself against each provider account that Atlas uses.

## Safety

- [ ] The send function has an allowlist of read verbs. It refuses POST, IHAVE, TAKETHIS, CHECK, ARTICLE, BODY, MODE STREAM and any unknown verb before anything is written to the socket.
- [ ] The password is read only from `NNTP_PASS`. It never comes from argv, and it is never printed, logged or written. AUTHINFO lines appear as `AUTHINFO PASS [redacted]`, and the output is checked once more for the password string. The username is not stored either.
- [ ] The probe stores no Subject, From or body text. HEAD results keep only header names. Message-IDs keep only their shape: the local part becomes `A` for letters and `d` for digits, punctuation stays, and the domain is kept.
- [ ] If authentication fails, the probe tries it only once per connection and makes no second login, so it does not lock the account.
- [ ] `XFEATURE COMPRESS GZIP` runs on a separate throwaway connection, followed straight away by QUIT, so compression is never used. XZVER asks for 2 articles, the probe records the code and discards the body.
- [ ] Every socket operation times out after 20 s. Each step catches its own errors and reconnects if needed. A 480 response triggers authentication once and a retry. Responses 500, 501, 502 and 503 are recorded and the probe moves on.

## Run it (one run per provider in Atlas's config)

Take the host, port and username from Atlas's provider entries. Put the password in the environment from your secret store; do not type it on the command line.

```sh
read -rs NNTP_PASS && export NNTP_PASS          # paste the password, then press Enter
python3 -I nntp_probe.py --host <provider host> --port 563 --user <account user> \
    --group alt.binaries.test --out results-<provider>.json
unset NNTP_PASS
```

For several providers, run the block above once for each provider in Atlas's config. Use a fresh `NNTP_PASS` for each run and give each run its own `--out` file, for example `results-primary.json` and `results-backup.json`. Don't run the probes in parallel: one connection at a time stays within the account's connection limit.

Options: `--no-tls` with `--port 119` for plain-text connections, `--insecure-tls` to skip certificate verification, `--no-compress-tests` to skip XZVER and XFEATURE, `--group` to use a group Atlas really indexes (recommended as a second run), and `--newnews-minutes`, `--over-count` and `--hdr-count`. The probe writes the results file with mode 0600 and prints a summary table at the end.

## What each result means for linking research

| Result | What it tells us |
|---|---|
| DATE skew | Server clock minus local UTC (about ±1 s plus RTT/2). Subtract it before comparing server timestamps with our own clock. |
| NEWNEWS code, count, seconds | A 230 response means the server can list IDs that arrived after time T. Bisecting T (for example 10m, 5m, 2.5m) finds an article's exact arrival time on that server, without relying on the poster-supplied Date. A 502 or 500 means the probe has to fall back to NNTP-Posting-Date or to polling GROUP high-water marks. The count is the arrival rate. The probe stores at most 2000 IDs and counts the rest. |
| LIST OVERVIEW.FMT, OVER Lines field | If `:lines`/`Lines:` is empty or 0 for every article (`Lines all empty/zero = true`), Atlas has to use `:bytes` for size matching and part-size heuristics, never Lines. |
| Date year digits | Shows whether Date fields have 2-digit years, which the Date parser must accept. |
| Msg-ID shapes and domains | Shows the format of the ID that the posting tool or provider generates. A fixed-length shape or digit run in the same position across articles can encode time, session or sequence (as in many powerposts). Compare the shapes across providers before trying to decode any field. A small number of distinct shapes means the IDs follow a template. Some patterns to look for: `d{13}` is a millisecond epoch, as in Nyuu's `([A-Z][a-z]){12}-d{13}@nyuu`. `d{10}` is likely Unix seconds. A fixed-length mixed-case run of 24 or 32 characters is a random-ID mode. The shape keeps no digit values, so it does not reveal the timestamp itself. |
| HDR / XHDR on NNTP-Posting-Date, X-Received-Body-CRC, Injection-Info, Path | These headers are not in the overview. If a header has `with_value` > 0 in the HDR result, Atlas can fetch it in bulk without HEAD. NNTP-Posting-Date gives the injection time. Injection-Info parameter names show whether posting-host or logging-data exists for grouping by session. X-Received-Body-CRC can confirm that two parts are identical across servers. The median Path length shows how many hops a post took. |
| LIST HEADERS | Which headers the server can serve through HDR. |
| LIST COUNTS / ACTIVE | Group high and low marks and the article count, for planning backfills. |
| HEAD header names | Which headers exist at all, including ones not in the overview. |
| STAT codes | 223 means the server can look up articles by Message-ID (cross-provider existence check). 430 means it cannot find the ID. |
| XZVER, XFEATURE | Whether the server offers compressed overview. 2xx or 290 means supported. Use this only to plan future work. |
| caps IMPLEMENTATION | Server software, which explains odd behaviour. |

## Paste results back (one column per provider)

| Item | Provider A | Provider B |
|---|---|---|
| IMPLEMENTATION / greeting | | |
| DATE skew (s) | | |
| OVER or XOVER; Lines empty? | | |
| Date year digits | | |
| Top Msg-ID shape (rle) + distinct count | | |
| HDR NNTP-Posting-Date with_value/lines (code) | | |
| HDR X-Received-Body-CRC with_value/lines | | |
| HDR Injection-Info params | | |
| HDR Path median hops | | |
| NEWNEWS code / count / seconds | | |
| STAT codes | | |
| XZVER / XFEATURE codes | | |
| errors | | |

You can share the results JSON file. It contains no secrets, Subjects or poster details, but it does contain the host name and the Message-ID domains.

## Offline test

`fake_nntp_server.py PORT [--legacy|--hostile]` is a fake NNTP server on localhost. Run it and point the probe at it with `--no-tls`.

- The default mode has full RFC 3977 support.
- `--legacy` returns 500 for OVER and HDR and 501 for LIST COUNTS, which tests the XOVER, XHDR and LIST ACTIVE fallbacks.
- `--hostile` greets with 201, returns 500 for CAPABILITIES, DATE and HELP, 503 for HDR and XHDR, and 430 for STAT, and sends a NEWNEWS list of 250,000 IDs, which tests the overflow handling and the reconnect.

`fake_tls_wrap.py` runs the same server behind a self-signed TLS certificate. `unit_test.py nntp_probe.py PORT` checks the verb refusals, the 480 authentication and retry, the timeouts and reconnect limit, and the overview parsing with a reordered OVERVIEW.FMT.
