# Wave 1 A: timing and cadence (agent ran 06:44-06:48Z)

Headline: the real case (random group per file, random From per article) matches ngPost --obfuscate with group_policy each_file. ngPost picks _grpList.at(rand()%nbGroups) per file and randomStdFrom() per article (13 alnum @ msgid-signature .com). ngPost sends NO Date header, so Date = server arrival time and the files form one continuous time stream over 4 groups. https://github.com/mbruel/ngPost (NgPost.cpp, PostingJob.cpp, NntpArticle.cpp)

1. ngPost: From domain == Message-ID RHS (<uuid32@ngPost>, xxx@ngPost.com; -z changes both). In obfuscated mode Subject == Message-ID local part. (obscurity 4, high)
2. Nyuu Message-ID = rand24 (alternating case) - 13-digit ms epoch @nyuu; ms ordering across files/groups; cluster on gap < ~2s. --message-id overrides. https://github.com/animetosho/Nyuu lib/article.js (4, high)
3. Nyuu --date now: one Date for the whole run. Otherwise toUTCString() "GMT". (4, high)
4. GoPostStuff <UnixNano$gps@gopoststuff>; newsmangler <epoch.5dp.partnum@host>. https://github.com/madcowfred/GoPostStuff https://github.com/madcowfred/newsmangler (5, high, old tools)
5. Date format per tool: ngPost none (server adds), Nyuu GMT, postie RFC1123Z +0000; postie full obfuscation RandomDateWithinLast6Hours(), so Date is 0-6h before arrival. https://github.com/javi11/postie (5, med-high)
6. Arrival time from article numbers: keep per-group (article number -> wallclock) curve from every high-water-mark scan; join groups by overlapping arrival intervals and bytes/sec. nZEDb postdate() does number->date lookups. (3, medium)
7. nxg-upper: Message-ID = sha256("NXGHEADER:data|par2:n") formatted 40@21.3 hex; header lets you regenerate all IDs and encodes counts. https://github.com/Tensai75/nxg-upper . postie: rand32@rand8.rand3, From rand14 <rand14@rand5.rand3>. (5, high)
8. File boundary interleave: N connections from one queue, so file k+1's first parts arrive before file k's last; retries show as late out-of-order parts (Nyuu new Message-ID per retry). (3, medium, inferred)
9. Prior art: nZEDb CollectionKey = cleaned subject + group id + file count; MGR mode drops group id. No indexer groups by timing. (high)

Header availability: XOVER = Subject, From, Date, Message-ID, References, bytes, lines, Xref. NNTP-Posting-Date, Injection-Date, Path, User-Agent, X-Newsposter need XHDR/HEAD.
Next: per-tool Message-ID/Date fingerprint table incl JBinUp, PowerPost, NewsUP, postie ngx; test which providers return Path/Injection-Info via XHDR; prototype arrival-curve join.
