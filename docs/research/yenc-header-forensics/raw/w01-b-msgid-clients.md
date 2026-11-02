# Wave 1 / Agent B: Message-ID and posting-client fingerprints (Haiku)
- newsmangler: Message-ID `<%.5f.%d@HOST>` (unix time with 5 decimals, part number, server hostname), no randomness. Same host plus clustered times = one session; the time orders parts. In OVER. (4/5) src: github.com/madcowfred/newsmangler postmangler.py
- newsmangler Subject `%s [%s/%d] - "%s" yEnc (%s/%d)` zero-pads to the digit width of the totals, so the pad width shows batch size. (4/5)
- newsmangler X-Newsposter: `newsmangler <ver> (<yenc mode>) - url`, HEAD only. (2/5)
- newspost: subject modes -s/-y/-q/-e; X-No-Archive: yes by default; -X custom headers persist via -d. HEAD. (3/5)
- ngPost: Message-ID domain = --msg_id signature, default "ngPost"; a retry uses a new UUID; obfuscated subject = UUID. Random From by default (format undocumented). OVER. (3-4/5)
- Nyuu: Message-ID template e.g. `${rand(24)}@DOMAIN`, with a forced constant length; uploader-set domain is constant per uploader. From default is `username <username@host>` from the local system. (3/5; defaults unverified)
- NewsUP: generate_ids option; uploader default `NewsUP <NewsUP@somewhere.cbr>`; extraHeaders become X-extra-header-*. (2-3/5)
- RFC 5536: msg-id at most 250 octets; id-right is case-sensitive; User-Agent rules. X-Newsposter is not in the RFC.
Leads: Nyuu src message-id/from generators; ngPost --gen_from format; NewsUP generate_ids length.
