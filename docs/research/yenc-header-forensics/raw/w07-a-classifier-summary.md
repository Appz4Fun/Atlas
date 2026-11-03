# w7a: Message-ID classifier over server-ids/segs.tsv

Corpus: 72,334 segments, 246 NZBs. Columns (no header): nzb-id, nzb date (unix), Message-ID, Message-ID host (col 4, not a tool label), From (col 5). list.txt = 1,989 source paths in public GitHub repos (many are test fixtures). 2,184 rows have no "@" (col 3 holds a filename-like string, e.g. 1G.bin?1=0:500000, poster "nserv").

## (1) Counts (segments / distinct NZBs), first-match order
nyuu 23,831 / 103 (strict grammar); private (-13digits@PRiVATE) 11,628 / 5; unknown 10,370 / 87; ts_host_random (hex-ish@ts.rand) 4,032 / 4; ngpost 3,689 / 2; yencbin_partN (Part N of M.HEX32@ts.local) 2,816 / 4; usenetfarm (32 alpha @UFK / @news.usenet.farm) 2,775 / 2; alnum_rand_host 2,753 / 2; no_at_sign 2,184 / 9; nyuu-shaped prefix with other host (NNNNNNN, nothereatall) 1,826 / 2; powerpost 1,572 / 2; hex3_dollar_7 1,396 / 1; diablo 1,218 / 10; newsmangler 895 / 5; highwinds 869 / 7; synthetic_a1 212 / 8; hex32-ts@host 138 / 4; usenetfarm_loose 116 / 1; giganews_other 10 / 1; nyuu_loose 4 / 4.
Zero hits: gopoststuff, postie, nxg, jbinup, inn (refined), giganews (strict), nntpposter. The spec's inn grammar matches none; the real "$"-separated hex forms fall under diablo or hex3_dollar_7.
About 442 rows use placeholder hosts (example*, news.example, nntune.test, nothereatall). These look like fixtures, not live posts. INFERRED.

## (2) Top 15 unknown shapes (a=lower run, A=upper, 9=digits, x=mixed case, h=hex>=8; {n}=run length)
1. 52 h{15}@a{4}.a{7}.a{3} e.g. 123456789abcdef@news.newzbin.com (fixture)
2. 52 x{29}@x{9}.x{3} e.g. D3q02bY0SDsDBnoqoveopn2fKvat3@H20ZLaQbk.ZmQ
3. 39 x{11}@x{32} e.g. ZmmnLr1Rnns@0899bfc3031c515c05SAxJeecaB9xxMI
4. 38 x{19}@x{32} e.g. 7HaqkK3BCVroUYmobGb@8f315c53cc3901f00HvBpo5KnBKJHMSg
5. 38 x{14}@x{30} e.g. SkZiPjojP7MUar@33c09c501f0c5d710BnOk9Pa4hXrtj
6. 38 a{9}@a{8}.a{4} e.g. zikeqsy4p@i3i47n5m.hpd6
7. 37 x{20}@x{32} e.g. h9DwmS80fiVhv2eJqp3W@8f3c039f5b13c00c1QFwLt1SZH7G3Epq
8. 36 x{12}@x{36} e.g. vg3CooxFhx8i@153c00c871f794301fTZ4fcpVFA4EekVVHAA
9. 36 x{20}@x{28} e.g. 04JcF12RfVKV8sREuX5E@08c7f5cc91013c034ovxaBzNMOTr
10. 36 x{11}@x{33} e.g. V29GvfiDaMk@ccf9261ac53300101qjBqWWve8jdUSw96
11. 36 a{6}@a{6}.a{2} e.g. ttb8wy@er4pp5.0s
12. 35 x{14}@x{29} e.g. sr6NsO1myVtkXX@3145103c8108cf0094Nk9BDbC8Quf
13. 35 x{11}@x{36} e.g. hcjMxOTiJ4S@08190671cc135f035xjhaXklTZiRaF7G9umN
14. 35 x{13}@x{35} e.g. CDQrQrtj9PAVj@0fc09c05351c831cdc1vEQSr5JLnmuQiIsq
15. 35 x{15}@x{31} e.g. Q1zt7NXsyYoKhBt@56563cf1a0c103809PEk1uK7aJHK7Cp
Length-stripped, the 10,370 unknowns collapse to 1,861 shapes. Top: x@x (3,269), a@a.a (2,420), x_x@x.x (179), x-x@x.x (172). The dominant unknown is random-alnum@hex-or-alnum host with variable lengths, a family not in the spec. Several are probably a single unlisted generator. INFERRED.

## (3) Highwinds (869 segs, 7 NZBs; format A.N$B.K@fxNN.host)
- Per NZB: c857a4ff7e n=655 (fx33.iad), N gap 24.0%, K median 124,335; 3fedd762a4 n=185 (fx30.am4), gap 0%; c7c1206442 n=4, gap 42.9%; f5fb8db749, 939b1d73d5 n=11 each, gap 0%; a81258bd56 n=2; 49e9826bc0 n=1.
- T1 decode: value = sum(idx(c_i)*64^i) over the prefix A, little-endian, +876645500. Alphabet 0-9a-zA-Z plus '_'=62 and '%'=63. The two unknown symbols are inferred from frequency. Each symbol appears about 10 times across 869 prefixes, which supports a 64-symbol base. With '_' before '%' there are 0 decreases in decoded time across N order (862 pairs). With the reverse order there are 10. VERIFIED for monotonicity, INFERRED for the exact symbol indices.
- Decoded time minus NZB date: c857a4ff7e -865..-364 s; 3fedd762a4 -10..+47 s; the small NZBs 0..+7 s. So T1 is consistent with posting time just before the NZB date, but the 3fedd762a4 set has a few posts 10-47 s after the NZB date. Unexplained.
- Diablo check (for comparison): hex8 prefix as unix minus NZB date: median -17 s, n=1,218, with an outlier up to +3,655 s. VERIFIED for most rows.
- Nyuu: the 13-digit ms timestamp minus NZB date has median +1 s, max +93 s (a few outliers). VERIFIED.

## (4) K reuse across NZBs
869 K values (trailing number in the Highwinds ID, range 184..250,106) are all distinct. 0 values repeat across NZBs. VERIFIED. No account/connection reuse signal in K. The per-post K is not a constant per NZB, so it does not look like a per-session constant. The "K count" in the brief is read as posts per NZB. INFERRED.

## (5) From / poster patterns per class
- nyuu: "blablamannetje <blabla@example.com>" 23,182 of 23,831; "msft@noc.msft" 604. This is a placeholder address, not a default-leak. VERIFIED.
- private: "WtF[NZB] <freestuff@for.all>" 7,822; "nEwZ[NZB] <pr3d@NET.world>" 3,806. Two fixed names.
- ngpost: "Sc5dLs_aLaM5Mn-Ouwr@JxB048gTyZ.qDa" 2,140; "yv0Awps1AyT9U@ngPost.com" 1,549. Random local@random host.
- yencbin_partN: "yEncBin@Poster.com (yEncBin)" for all 2,816.
- powerpost: "VMrbkz70964ds70-. (X265_PAR2_35%)" 1,365; ". (X265_PAR2_35%)" 198; "Duik@boot.nl (Submariner)" 9.
- diablo: AutoMasterShare@gmail.com 992; Yenc@power-post.org 202; spitch@ratingshell.com 15.
- newsmangler: r@ndom.tv (r@ndom) 613; TheHiDefGuy 197; AutoMasterShare 84.
- highwinds: pJVEXc6ADv_fRH1Hfn-5jr@jlGt68LE.twc 655 (the big NZB); blabla@gmail.com 185; nzbtester* test accounts.
- hex3_dollar_7: "Winston Ho" 1,396 (a plain name).
- ts_host_random: "bd432ea78c21c132e87d167117355297 <grC..." 4,032 (one fixed hex-looking display name).
- usenetfarm: "NewsUP <NewsUP@somewhere.cbr>" 2,582 (ties to the spec's "NewsUP letters-only From" flag; no letters-only match was run separately).
- alnum_rand_host: "7cc92@648075182f.com" 1,716 (54 distinct Froms).
- Only two Froms are shared across a whole class, so per-class poster patterns are strong. Several classes use fixed display names from test corpora.

Caveats: no NZB was hand-checked. "unknown" grammars are INFERRED from shape only. Placeholder hosts indicate fixture data, so results generalize to the 246 NZBs, not to live Usenet traffic.

## Orchestrator correction
The agent's "K" section analysed the TRAILING number (R), which is random and expected to be distinct. Recomputing the real $K token per NZB on the same corpus gives exactly ONE K per NZB across all 7 Highwinds NZBs, all distinct: JP7/fx09.am4 (4), 6aQa/fx02.iad (1), 8E2/fx33.iad (655), 4Z1/fx31.am4 (11), BA5/fx21.am4 (2), EN/fx30.am4 (185), rY1/fx45.am4 (11). So K is still a per-post/session key. The '_'=62, '%'=63 ordering gives 0 time decreases over 862 consecutive pairs.
