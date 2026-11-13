# Historical storage source preservation

The old unregistered frozen payload/SQLite fixture files and naming maintenance
walker are removed from the shipping source tree. They have no current native
caller; pure historical format support remains in the accepted `history_stream`
decoder, local blob codecs, and independent NXV/overview tools. The original
freeze implementation and fixture source are preserved in Git at
`c62d0701acd07849014cd96161a0c99f2623212e`; the bounded private seven-file source
archive SHA256 is `9be433b93c5c6bf11a0b27557f8d23901690c63e975abea01284a6dc3356b649`.
The walker remains recoverable in the parent Git history.

No source file is hidden behind a backend feature or test flag. Retiring these
old implementations does not remove historical data, original caches, assets,
recovery copies or supported streaming parsers. No data file was opened or deleted.
New graph build/tests and actual preservation parity remain unqualified here.
