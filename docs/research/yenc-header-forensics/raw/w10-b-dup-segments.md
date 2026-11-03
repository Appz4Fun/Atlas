# W10-B: duplicate, gap and out-of-order segments in SABnzbd and NZBGet

Sources: raw.githubusercontent.com only (gh API returned 403 for both repos; add_repo not run).
- SABnzbd develop: sabnzbd/nzbparser.py (505 lines), sabnzbd/nzb/file.py. sabnzbd/nzbstuff.py is 404 on develop and master; NzbFile now lives in sabnzbd/nzb/file.py.
- NZBGet develop and master (identical 14628-byte copy): daemon/queue/NzbFile.cpp, NzbFile.h. daemon/nzb/*.cpp is 404; parsing is in queue/NzbFile.cpp.
- Not checked: nzbhydra2, nntmux, and SABnzbd nzbparser.py on master.

| Case | SABnzbd | NZBGet |
|---|---|---|
| Same number, different Message-ID | Keeps FIRST. Second is dropped, counted as bad_articles "duplicate_articles" | Keeps LAST. Earlier entry is silently replaced |
| Same number, same Message-ID | Keeps first, logs "Skipping duplicate article" | Keeps last (no visible effect) |
| Fallback to 2nd entry if 1st missing | No. One article slot per number, no alternate IDs stored. Exception: a 1st entry rejected by the size check is never stored, so a later valid entry with the same number is accepted | No. One slot per number, last one wins |
| Gap (number absent) | No placeholder. Only present numbers stored; file_bytes = sum of present segments. No error | Slot left null, then erased in AddFileInfo; missedArticles counted, size excludes it. No error |
| Out of order | Sorted by part number before NzbFile is built | Stored at index partNumber-1, so file order is irrelevant |
| Errors/counters | duplicate_articles, bad_articles (size <=0 or >=8 MiB) | None. partNumber <= 0 ignored |

Line references:
- SABnzbd nzbparser.py:449 `partnum = int(segment.attrib.get("number"))`
- SABnzbd nzbparser.py:454-459 `# Duplicate parts?` / `if partnum in raw_article_db:`; 456-461 logs "Duplicate part %s, but different ID-s" and calls nzo.increase_bad_articles_counter("duplicate_articles")
- SABnzbd nzbparser.py:466-471 `elif segment_size <= 0 or segment_size >= 2**23:` then `raw_article_db[partnum] = (article_id, segment_size)`
- SABnzbd nzbparser.py:483 `raw_article_db_sorted = [raw_article_db[partnum] for partnum in sorted(raw_article_db)]`
- SABnzbd nzb/file.py:117-119 first article popped, marked lowest_partnum (used for duplicate-NZF check); 168-173 add_article appends to decodetable
- NZBGet queue/NzbFile.cpp:425-429 `if (partNumber > 0)` then AddArticle
- NZBGet queue/NzbFile.cpp:52-61 AddArticle: `(*fileInfo->GetArticles())[index] = std::move(articleInfo);` (resize to index+1 first)
- NZBGet queue/NzbFile.cpp:81 `fileInfo->GetArticles()->erase(it);` (null gap entries removed)

Meaning for Atlas:
- A merged NZB with two Message-IDs for one segment number gives one article per number in both clients. The second ID is not a backup in either. SABnzbd discards it (counter only). NZBGet keeps whichever entry comes last in the file, so the client that gets the file decides which ID is used.
- If the chosen ID is missing on all servers, the file gets a hole (gap), not a retry on the alternate ID. Retries in SABnzbd are per server (nzb/article.py TryList), not per ID.
- Alternate IDs listed as "backups" in one NZB therefore do nothing as backups. If backups are wanted, they need a different mechanism (for example a separate NZB or a documented client feature), not duplicate segment numbers.
- Gaps are accepted silently by both clients. A merged NZB with missing numbers produces incomplete files that need PAR2; nothing warns at add time.
- SABnzbd's md5sum (nzbparser.py:452) hashes every article ID including duplicates, so duplicates change the NZB-level hash.
