# Wave 1 C: clustering, graph methods, server propagation (agent ran 06:44-06:47Z)

1. Takedown co-removal fingerprint (obscurity 5). DMCA notices list a release's Message-IDs, so its articles vanish together. STAT probes over time on several providers give per-article removal vectors; matching vectors = one post. OS3 2015 study (3.2M filesets): one article predicts the whole fileset; removal lag ~25h Eweka to ~95h Astraweb; resellers track their backbone (corr 1.00). https://rp.os3.nl/2014-2015/p82/report.pdf . After-the-fact only.
2. Injection-Info / X-Trace session fingerprint (4). INN 2.6+: posting-host, logging-data = nnrpd PID (one per connection), posting-account. Constant across per-file From changes. Not in XOVER; big backbones often strip it. https://lists.isc.org/pipermail/inn-workers/2009-December/017367.html
3. Cleanfeed EMP "do_phl" keys (3): same posting host + same body size in a window. yEnc parts of one file have identical sizes except the last, so (Bytes/Lines bucket, host, window) clusters across groups. Bytes/Lines are in XOVER. https://www.ibiblio.org/gferg/ldp/man/man8/cleanfeed.8.html
4. Xref arrival adjacency (4): per-group article numbers are arrival order; one upload session over 4 groups = simultaneous bursts; join bursts by start/stop/rate. True crossposts share a Message-ID with multiple Xref entries.
5. Kleinberg burst / change-point segmentation per group (3); match bursts across groups by rate and boundaries.
6. Sockpuppet co-activity graph (4): "An Army of Me" (WWW 2017) https://arxiv.org/pdf/1703.07355 . Nodes = From values, edges = co-posting within seconds across groups; Louvain/Leiden merges per-file posters.
7. Path tail + Message-ID domain = injection point (2). Blocking key, not a linker alone.
8. Fellegi-Sunter probabilistic linkage over 1-7 with EM-learned weights; block on time window + size (2).

Gaps: no public spec of Highwinds/Omicron extra headers; Nyuu/ngPost Message-ID formats unconfirmed.
Next: measure propagation/co-removal ourselves; fingerprint tools from Message-ID local-part shape and Date-vs-Xref order; survey which session headers survive on each backbone.
