# Wave 2 merged (07:17-07:20Z)
Agents again finished in 1-1.5 min (11-22 calls). Weak wave, but it settles some things.
- No verified HEAD-only twin key. X-Received-Body-CRC/X-Received-Bytes have no public definition. A live HEAD test on two known byte-identical reposts in Atlas would settle it in minutes. (Next wave searches archives that show full headers.)
- Date string in XOVER is a cheap tool cluster key: "-0000" (Python formatdate default), "+0000", "GMT" (obsolete), "UTC" (invalid). ngPost sends no Date (server sets it). Nyuu --date now pins one Date per run.
- Subject and name mismatch: PowerPost subjects lack the "yEnc" tag. Obfuscated posts have =ybegin name= different from the subject, so link on name=.
- NZB: spec default iso-8859-1 + newzBin DOCTYPE, while Nyuu writes utf-8 and can minify/compress, so a posted NZB fingerprints its writer. A shared password meta = same uploader. With ngPost obfuscation, the NZB poster is the only stable identity.
