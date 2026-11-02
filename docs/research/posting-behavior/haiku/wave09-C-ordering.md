# Wave 09 C: server-side ordering to rebuild posting timeline (Date is fake)

Tags: [src] verified in source (file:line), [rfc] verified in RFC text fetched, [docs] project docs, [forum] forum/mailing-list claim, [guess] my inference.

## 1. Article number assignment

- [src] INN innd/art.c:993: `data->Arrived = Now.tv_sec;` (arrival time is the server clock; Date is not used for ordering).
- [src] INN innd/art.c:1450-1481, `ARTassignnumbers()`: loops over GroupPointers (all groups of the article), and for each: `ngp->Last++` then `ngp->Filenum = ngp->Last`. One call assigns one number per group, and the Xref is built in the same call.
- [src] INN innd/art.c:1452-1453 comment: if the write later fails, numbers become "holes" in the active file.
- [src] INN innd/art.c:2569 calls ARTassignnumbers from ARTpost; ARTpost is reached from both IHAVE and TAKETHIS (innd/nc.c:245, art.c:2590).
- [src] INN innd/ng.c:385 and :440 reset `ngp->Last` from himark/lomark. Active-file reloads or renumbering can move the high-water mark, so strict monotonicity is not guaranteed over a server's lifetime.
- [rfc] RFC 3977 section 6: "articles arriving later MUST have higher numbers than those that arrive earlier" (per server, per group). Numbers are local: the same article may have different numbers in each group and on each server. Gaps and reinstatement are allowed.
- [forum] inn-workers 2002 (lists.isc.org/pipermail/inn-workers/2002-March/009026.html): a large peer can OFFER articles earlier than it SENDs the bodies. One example showed about 3 s between the offer and the body. Out-of-order body arrival at a peer is therefore real, but this is one anecdote.
- [guess] Diablo (dreaderd) and commercial backbones: I did not verify their numbering code. Assume the same arrival-order principle per server, but check before relying on it.
- [guess] Peering batches (for example, a feed sending a burst of IHAVE) can make numbers reflect the peer's send order, not origin time. Each server's order is internally consistent but differs across servers.

## 2. Propagation delay

- [src] Nyuu config.js:69-71: `check.delay: 5000` ms before the first post check, `recheckDelay: 30000` ms, `tries: 2`. Nyuu lib/uploader.js:450-453 re-queues on failure. Nyuu's defaults assume about 5 s to first visibility and 30 s to a retry.
- [forum] Newsbin forum (forums.newsbin.com/viewtopic.php?p=202471): users say indexer NZBs can be posted before articles propagate, and that a retry about 10 minutes later usually succeeds. The default retry schedule covers about 5 minutes in total.
- [forum] Typical propagation between major backbones for binaries: seconds for most articles, with a long tail of minutes. I found no measured per-backbone figures for Highwinds, Omicron, UsenetExpress or Abavia. [guess]

## 3. Injection-Date

- [rfc] RFC 5536 section 3.2.7: "This header field MUST be inserted whenever an article is injected." Agents MUST NOT alter a pre-existing Date when adding it. Injection-Date may be later than Date. No rule requires relaying agents to preserve it. Precision is the RFC 5322 date-time syntax, where seconds are optional. [rfc/guess on optionality]
- [src] INN nnrpd/post.c:400-405: nnrpd adds Injection-Date only when the article has NO Message-ID and NO Date header. Postie posts carry both, so Injection-Date is normally absent for them. This is the most important finding.
- [src] INN nnrpd/post.c:449-451 comment: innd as a relaying agent does not check Injection-Date beyond parsing it.
- [docs] INN doc/pod/inn.conf.pod:1199-1203 (addinjectiondate): same Message-ID/Date restriction.
- [forum] INN inn-workers 2017 thread (lists.isc.org/pipermail/inn-workers/2017-September/018754.html): confirms the restriction; nothing on peering retention or seconds precision.
- [src] INN innd/art.c:1051-1071: if Injection-Date is present, innd sets Posted from it and rejects it if unparseable.
- [guess] Preservation through peering is probably kept (headers are relayed as-is), but I did not verify it for any backbone.
- [guess] Not in the default overview fields (Xref is added at storage/overdata.c:73; I did not read the full list). Plan on HDR/HEAD to read it.

## 4. Aligning group numbers across groups on one server

- [src] One ARTassignnumbers call assigns numbers for all groups of a crossposted article (art.c:1450-1481). Its Xref pairs (group_a:n_a, group_b:n_b) therefore share one arrival instant on that server. Each such pair is an exact anchor between the two group counters for that server.
- [guess] Build a monotone map per server between group counters from these anchors (isotonic regression or piecewise linear). Use it to place single-group posts from the same upload on a shared scale.
- [forum/guess] Prior art: I found no concrete source for nZEDb/NNTmux postdate fallback logic in the search results. Do not cite it without reading the code.

## 5. Bottom line

Article numbers give an exact order of arrival per (server, group), and crosspost Xrefs give exact anchors between groups on one server. Date is not used. Cross-server ordering needs propagation delay, which is probably seconds, with a minutes-long tail [guess]. Injection-Date is usually absent for Postie posts and is not a reliable fallback.

### Algorithm sketch

1. Pick one server S. For each article seen on S, record (group, n, Message-ID, Xref, subject/poster pattern).
2. Per group on S, sort by n. This is arrival order at S, not origin time.
3. From each Xref on S with two or more groups, take the pairs (group_a:n_a, group_b:n_b) as simultaneous events. Fit a monotone map f_ab from n_a to n_b.
4. Cluster articles into uploads by subject/poster/file-name pattern and by article-number proximity in each group.
5. For each upload, record the window [min n, max n] per group on S, mapped to a common clock through the anchors. Its position is the arrival window at S.
6. If HDR Injection-Date exists, use it as a coarse absolute time (seconds) to order uploads across servers. Ignore Date entirely.
7. Across servers, compare arrival windows of the same upload. The offset is about the propagation delay, expected seconds to minutes [guess]. Treat the result as ordering with error about the propagation delay, not as absolute time.
8. Expected error: within one server, ordering error is near zero apart from upload spread (the time to post all parts). Across servers, error is the propagation delay [guess]. Postie's 0-6 h Date randomization does not enter this estimate.

### Top 3 risks

1. Injection-Date is absent for Postie posts, since they include both Message-ID and Date (INN nnrpd/post.c:400-405). No timestamp fallback exists for most uploads.
2. Article numbers are local to each server. Cross-server comparisons need a propagation model. Out-of-order body delivery (forum claim) and gaps or renumbering (ng.c:385/440, art.c:1452) break monotonicity.
3. Anchor coverage: crosspost Xrefs may be missing or partial, and Diablo/commercial numbering is unverified [guess]. Without anchors, group counters cannot be aligned.
