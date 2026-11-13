# Retired one-off SQLite catalog maintenance

The fresh PostgreSQL catalog does not upgrade the old header/release catalog.
Product admission therefore retires the old bulk purge/reprocess, PreDB file-import
and raw replay CLI, naming normalization/undo/recount, repair-count migration and
repost-splitting and standalone SQLite CRC report command paths. They are rejected before configuration migration,
provider connections or database setup, including mixed command lines. No command
succeeds without doing its work. This is retirement of old product commands, not
native maintenance parity or authorization to delete old data.

The source retirement in this checkpoint removes `naming::{mojibake,recount,
reflag,unname}` and `repair_counts`; the walk/field-search CLI edge is retired with the same commands. `repost_split` retains every pure `Copy`/`Keep`/`Verdict` classifier,
constant, `decide`, `looks_reposted`, bounded provider-date lookup, and all ten pure
classification tests. Its SQLite selection/mutation and CLI are removed.

The retired inline tests are `naming::mojibake::tests::{each_double_encoded_row_folds_renames_or_stays,
a_renamed_row_takes_the_rest_of_its_post}` and
`naming::recount::tests::a_row_saved_since_gets_every_article_counted`. They cover
repair of historic SQLite catalog state. Native ingestion, naming, subject/codecs,
identity and typed mutation tests remain; these fixture retirements do not replace
or waive those current behaviors.

Old command integration targets (`naming_reflag`, `naming_unname`,
`naming_unname_later`, `pp_purge`, `pp_reprocess`, `predb_import`, repair/repost
reports) need explicit test-graph retirement with their old commands. Current
provider, identity, parser and grouping tests must instead keep or port their
behavioral assertions. Original source and fixtures remain in the frozen
pre-cutover Git history; no data files are touched by this source change.

All runtime/build/test checks are **UNRUN**. The changed frontdoor tests exercise
retired flags and no-side-effect admission; they make no claim to implement the
retired operations. Native configured workers, costly caches, NXV, backup/restore,
footer and README attribution remain required.
