# Wave 4 B: in-band manifests on Usenet (Haiku, ~19 calls)
1. Spotnet groups: free.pt (headers), alt.binaries.ftd (NZBs), free.usenet (comments), free.willey (reports). https://en.wikipedia.org/wiki/Spotnet (med)
2. Spot header: X-XML, X-XML-Signature, X-User-Key (RSA Modulus/Exponent), X-User-Signature; From = [Nick] <[MODULUS].[SIG]@[CAT][KEY-ID][SUBCATS].[SIZE].[x].[DATE].[CUSTOM]...>. https://github.com/spotnet/spotnet/wiki/Spot-Header-format (med-high)
3. Spot XML: <Spotnet><Posting> Key, Poster, Title, Tag, Created (unix), Size, Description, Image/Segment, Category, NZB/Segment = Message-ID of the zipped NZB article. https://github.com/spotnet/spotnet/wiki/Spot-Xml-format (med-high)
4. Key-IDs: 3 client "Spotnet", 2 moderator, 7 public. https://github.com/spotnet/spotnet/wiki/Public-Keys
5. NxG header: base64 of 21-char random + ":data_count:par2_count"; MID = SHA256("[HEADER]:[data|par2]:[n]") formatted [0:40]@[40:61].[61:64]. https://github.com/Tensai75/nxg-upper (high)
6. NZBLNK: nzblnk:?t=title&h=header&p=password&g=groups&d=date. Resolved by indexer search, or nzb-monkey-go "directsearch": scan groups g around date d for headers matching h and BUILD the NZB from headers. https://github.com/Tensai75/nzb-monkey-go , https://forums.newsbin.com/viewtopic.php?p=224615 (med). -> client-side linking algorithm = prior art to read.
7. No evidence NZBs are routinely posted to alt.binaries.nzb today.
Ground truth: verified spots -> NZB article -> authoritative segment MIDs + groups for one post.
