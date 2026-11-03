# Wave 2 / Agent B: historic posters and Date formats (Haiku)
- Python email.utils.formatdate default = "Fri, 09 Nov 2001 01:08:47 -0000" (-0000 zone); usegmt=True gives "GMT". VERIFIED docs.python.org. Date is in XOVER. (3/5)
- RFC 5322: "-0000" = local zone unknown, "+0000" = local is UTC. A cheap cluster key. cr.yp.to/immhf/date.html (3/5)
- "GMT"/"UT" are obsolete zones and "UTC" is invalid (IETF emailcore 2022). Node toUTCString -> GMT and chrono -> +0000 are UNVERIFIED. (2/5)
- Odd dates (2-digit years, "GMT+0100", "(EST)" comments) point to legacy or hand-rolled code. (3/5)
- yEnc 1.0 layout (July 2001 spec quoted at nntpstream.blogspot.com/2012/03/yenc-10.html): "=yend size= part= pcrc32= crc32=". The spec example itself has mismatched part numbers.
- Obfuscation: name= differs from the subject filename in ~80% of posts in some groups (Newsbin forum p=222552). Link on name= rather than the subject.
- PowerPost 2004 ignores the yEnc marker in the subject (body is yEnc, subject has no "yEnc" tag). (4/5)
- Pan User-Agent "Pan/0.14.2.91 (...)". 2005 survey of newsreader header names (x-newsreader, x-posting-agent...).
- UNVERIFIED: yEnc 1.2/1.3/yEnc2/=ycrc (yenc.org blocked).
Leads: Pan source; ydecode.com EasyPost help; another yenc.org mirror.
