# W1A PAR2 internals (Haiku, 06:57Z)
- File ID = MD5(hash16k, length, ASCII name); rename changes ID but hash16k+length+full MD5 stay. Twin key = hash16k+length.
- UniFileN overrides ASCII name; ID computed on ASCII name -> pitfall when names differ.
- IFSC packets: per-slice {MD5, CRC32}. With Main slice size, a decoded article payload (if slice-aligned) can be matched to a file slice. Twin from ~2 articles.
- SABnzbd par2file.py: hash16k dict, drops colliding hash16k, computes full-file CRC32 from IFSC slice CRCs via crc32_combine (adjusting last slice).
- Creator packet: tool string, not in ID; tool-family hint.
- Slice size per set; PkdMain subslice size rare -> tool fingerprint. ParPar defaults (sliceSizeMultiple 4, pow2 output sizing). RecvSlic exponent schemes differ per tool.
- Vital packet repetition/order per tool -> weak fingerprint.
Leads: IFSC slice-MD5 vs article payload; RecvSlic exponent scheme; PkdMain/pow2 + UniFileN mismatch.
Sources: parchive spec article-spec.html; sabnzbd par2file.py; libpar2 descriptionpacket.cpp; github animetosho/ParPar
