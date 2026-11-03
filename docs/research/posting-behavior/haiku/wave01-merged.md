# Wave 1 merged (06:44-06:50Z)
Agents finished in 1-2 min each with 9-11 calls, well under the 10 min cap. Wave 2 asks for deeper digging (source code, ~25 calls).
## Strongest leads
- Per-run constants (cheap, high confidence): decoded part length from =ypart begin/end, yEnc line length, Message-ID shape/host (Nyuu `rand24-ts@nyuu`, ngPost `uuid@ngPost`), subject template. Combine into a "posting-session fingerprint" that is group-independent.
- ngPost GROUP_POLICY "one group per file" is a documented cause of the Batman-style split. Grouping must never require a single group; key on the session fingerprint + subject stem + time window instead.
- ngPost -x: Subject == Message-ID local part. Nyuu --date now: identical Date across a whole run.
- Message-ID RHS / Path first hop = injection point; Date-vs-NNTP-Posting-Date skew constant per session (needs validation).
- PAR2 per-file MD5 as cross-group join key (already parsed in src/par2.rs).
## Weak / unsourced
- Article-number contiguity, DBSCAN on Date, change-point on rate: methods only, no sources.
## Dead ends
- GitHub API blocked for subagents; use raw.githubusercontent.com. ResearchGate unfetchable.
