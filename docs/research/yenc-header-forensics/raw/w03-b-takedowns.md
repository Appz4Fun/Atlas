# Wave 3 / Agent B: takedown notices as a linking oracle (Haiku)
- DMCA notices must list subject, date, message-ID(s) and group (easyusenet.nl/takedownnotice.html, fastusenet.org/proper_dmca.html). No sample notice was found that lists a release's reposts. Proves nothing alone.
- NewsDemon removes the named articles one by one, not per release (newsdemon.com/usenet-takedowns).
- Lumen: researcher API keys only, by email, for non-commercial research. No Usenet notices were found. Effectively off-limits.
- UvA study rp.os3.nl/2014-2015/p82/report.pdf: XOVER polling every 5 min plus STAT probes, 3.2M filesets, 382M STATs on a.b.boneless. Articles of a fileset are removed together, so one STAT per fileset is enough. Co-removal correlates across providers (Hitnews/USENET 1.00, Eweka/Bulknews 0.88, Giganews ~0). Removal starts ~18h after posting; mean 1 day (Eweka) to 3.6 days (Giganews). STAT error ~0.03%. (4/5)
- Idea: STAT co-removal timing across groups = same release (probable, not a twin). Cost: a few STATs per candidate per hour for ~1 day. Note: Atlas must still never purge partial releases; this is only for linking.
- BREIN uses OSINT, not header scans. MPA/ACE and Rightscorp methods were not found.
