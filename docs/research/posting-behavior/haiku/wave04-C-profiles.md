# Wave 4 C: long-term uploader profiles (Haiku)
1. Timeprints (Johansson, Kaati, Shrestha, Security Informatics 2015) https://link.springer.com/article/10.1186/s13388-015-0022-z : 24 hour + 6 period-of-day + 7 weekday + weekday/weekend + 12 month bins; cosine for alias matching; time-only top-1 100%@500 users, 89.6%@2000, 70.6%@4000; needs ~500 posts for >80%, ~800 for >95%. Period-of-day and weekday/weekend most important. Synthetic alias split; no TZ/bots.
2. Temporal fingerprints (Somin, Erhardt, Pentland, arXiv:2407.04350, 2024) https://ar5iv.labs.arxiv.org/html/2407.04350 : inter-event gap distribution + two-sample KS; AUC ~0.78, 96% precision top-100; >=20 events per profile.
3. Deep metric account linking (arXiv:2105.07263): text + hour + subreddit; time is an add-on.
4. Cuevas et al. CoNEXT 2010 (https://arxiv.org/pdf/1007.2327): BitTorrent publishers linked by username + seeder IP; rate/day, lifetime, parallel torrents; 55% of top IPs -> one username.
5. VendorLink arXiv:2305.02763: style only, no timing.
Pitfalls: Date can be spoofed/jittered (Postie full obfuscation: now - U(0,6h)) -> use server arrival time; TZ shift (circular shift of 24-bin histogram, own suggestion); cron uploaders flat; shared tooling.
Recommended: per-uploader-candidate vector = period-of-day(6) + weekday(7) + weekend(2) + hour(24, circular shift) + gap quantiles + daily volume mean/var + release-to-post latency vs PreDB + tool settings mode. Stage 1 cosine top-10; stage 2 KS on gaps + tool match. Needs hundreds of posts -> uploader-level linking (same uploader), not slice-level.
