# Wave 1 C: temporal clustering and propagation (Haiku)
1. Path-graph signature (first hop + Path suffix) per window. https://www.measurement.sk/M2009/proceedings/214_Surmacz.pdf (2009 NNTP traffic measurement; propagation left as future work) (med)
2. Message-ID RHS = injection server; group by host + time bucket. https://www.giac.org/paper/gcfa/2676/forensic-primer-usenet-evidence/114938 (med)
3. Per-session constant skew: NNTP-Posting-Date (or first-seen) minus Date; cluster on delta mode. https://datatracker.ietf.org/doc/html/draft-ietf-usefor-article-08 (med-low)
4. [hypothesis] article-number contiguity runs per (From, Newsgroups), gap <50 (low)
5. [method] DBSCAN / gap threshold on sorted Date, eps from median inter-arrival (low)
6. [method] CUSUM/PELT change-point on per-poster rate to split reposts (low)
7. yEnc name+total+size equality. https://www.harley.com/usenet/usenet-tutorial/how-binary-files-are-handled.html (high)
Follow-ups: RFC 5536 / INN for Injection-Date, NNTP-Posting-Date; "Today's Usenet Usage: NNTP Traffic Characterization" paper.
