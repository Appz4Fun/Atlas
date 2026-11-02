# w02-c: Atmos / DTS:X / AAC detection rules
Read 2026-11-02. Quoted conditions were read from the cited source. Items marked UNVERIFIED were not confirmed against a real file.

```yaml
- field: eac3_atmos_addbsi_flag
  where: E-AC-3 bsi, addbsi block (present when addbsie=1)
  parse: |
    FFmpeg ac3_parser.c: if (get_bits1(gbc)) { addbsil=get_bits(gbc,6);
    for (i=0; i<addbsil+1; i++) { if (i==0) { skip_bits(gbc,7);
    eac3_extension_type_a=get_bits1(gbc);
    if (eac3_extension_type_a) { complexity_index_type_a=get_bits(gbc,8); i++; } }
    else skip_bits(gbc,8); } }
    Flag = LSB of addbsi byte 0; complexity index = byte 1.
  use: Atmos flag (flag==1) and ComplexityIndex. Tag candidates.
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/ac3_parser.c

- field: eac3_atmos_profile_ffmpeg
  where: ac3dec.c, after a frame decodes without error
  parse: 'avctx->profile = s->eac3_extension_type_a == 1 ? AV_PROFILE_EAC3_DDP_ATMOS : AV_PROFILE_UNKNOWN;'
    profiles.c name: "Dolby Digital Plus + Dolby Atmos"
  use: label only; same signal as addbsi flag.
  source: ac3dec.c, profiles.c (FFmpeg master)

- field: eac3_joc_emdf
  where: E-AC-3 frame, after audblk; EMDF sync 0x5838 is scanned for in the frame bytes
  parse: |
    MediaInfo File_Ac3.cpp: 'while(Search.Remain()>18){ if (Search.Peek2(16)==0x5838 && Ac3_EMDF_Test(Search)) break; Search.Skip(1); }'
    Ac3_EMDF_Test: emdf_container_length=16b; emdf_version=2b, must be 0; key_id if 3b==7;
    payload_id=5b (0x1F -> +5b); payload 14 = "JOC" (dispatch 'case 14: joc(); break;'), 11 = object_audio_metadata.
    joc_header(): joc_dmx_config_idx 3b (skipped), joc_num_objects_bits 6b -> joc_num_objects=bits+1, joc_ext_config_idx 3b.
    HasJOC = 'joc_num_objects_map.size()==1 && (count >= Frame_Count_Valid/8 || count >= Frame_Count/8)'
  use: Atmos (JOC) + object count. Needs many frames: one frame is not enough. Some Atmos streams have addbsi flag but no EMDF JOC, and the reverse; check both.
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Ac3.cpp

- field: eac3_joc_labels_mediainfo
  parse: HasJOC -> profile "E-AC-3 JOC", commercial "Dolby Digital Plus with Dolby Atmos". Complexity index = addbsi byte 1 when addbsi flag bit set (File_Ac3.cpp).

- field: truehd_atmos_flag
  where: TrueHD major sync (after the 0xF8726FBA sync). Fields in order: is_vbr 1b, peak_bitrate 15b, num_substreams 4b, skip 2b, extended_substream_info 2b, substream_info 8b.
  parse: |
    FFmpeg mlp_parse.c: 'mh->num_substreams=get_bits(gb,4); skip_bits(gb,2); mh->extended_substream_info=get_bits(gb,2); mh->substream_info=get_bits(gb,8);'
    FFmpeg mlpdec.c: '/* If there is a 4th substream and the MSB of substream_info is set, there is a 16-channel spatial presentation (Atmos in TrueHD). */
    if (m->avctx->codec_id == AV_CODEC_ID_TRUEHD && m->num_substreams == 4 && m->substream_info >> 7 == 1) { m->avctx->profile = AV_PROFILE_TRUEHD_ATMOS; }'
    MediaInfo File_Ac3.cpp (0xBA): 'Get_SB(HD_HasAtmos, "16-channel presentation is present")' = MSB only, no num_substreams test.
    If HD_HasAtmos: 16ch_channel_count 5b (+1 = object count) sits behind 7 unknown bits + 1b flag.
  use: TrueHD Atmos flag and object count. FFmpeg and MediaInfo disagree when num_substreams != 4 and the MSB is set. UNVERIFIED which matches real discs.
  source: mlp_parse.c, mlpdec.c (FFmpeg master); File_Ac3.cpp (MediaInfoLib master)

- field: dtsx_xll_profile
  where: DTS-HD XLL extension, extradata_syncword (dword-aligned, read at the start of the XLL extradata)
  parse: |
    FFmpeg dca_xll.c: 'extradata_syncword = show_bits_long(&s->gb, 32);
    if (extradata_syncword == DCA_SYNCWORD_XLL_X) { s->x_syncword_present = 1; ... }
    else if ((extradata_syncword >> 1) == (DCA_SYNCWORD_XLL_X_IMAX >> 1)) { s->x_imax_syncword_present = 1; }'
    Profile: 'if (s->x_imax_syncword_present) avctx->profile = AV_PROFILE_DTS_HD_MA_X_IMAX; else if (s->x_syncword_present) avctx->profile = AV_PROFILE_DTS_HD_MA_X; else AV_PROFILE_DTS_HD_MA;'
    dca_syncwords.h: XLL_X=0x02000850, XLL_X_IMAX=0xF14000D0.
  use: DTS:X (legacy and IMAX) flag. Core-only DTS: dca_core.c sets only HRA, ES, 96_24 or DTS, never X.
  source: dca_xll.c, dca_syncwords.h (FFmpeg master)

- field: dtsx_mediainfo
  where: File_Dts.cpp, profile fill
  parse: 'Data[Profiles].push_back("IMAX")' if Presence[presence_Extended_IMAX]; 'Data[Profiles].push_back("X")' if Presence[presence_Extended_X];
    Codec "DTS-HD" if profile MA or HRA, else "DTS". Comment in file: "A DTS:X extension sync word only announces DTS:X metadata. It does not prove that the presentation contains dynamic objects. Probe the CRC-valid primary type-241 presentation metadata."
  use: Presence of the sync word does not prove dynamic objects. Use type-241 CRC-valid metadata for an object count.
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Dts.cpp

- field: aac_audio_specific_config
  where: MP4 esds DecoderSpecificInfo; MKV A_AAC CodecPrivate (same ASC; not checked)
  parse: |
    FFmpeg mpeg4audio.c:
    object_type = get_bits(5); if (object_type==AOT_ESCAPE /*31*/) object_type = 32 + get_bits(6)
    sampling_index = get_bits(4); rate = (index==0x0f) ? get_bits(24) : table[index]
    chan_config = get_bits(4)
    if (object_type==AOT_SBR || (object_type==AOT_PS && !(show_bits(3)&0x03 && !(show_bits(9)&0x3F)))) { ext_object_type=AOT_SBR; sbr=1; ext_rate=get_sample_rate(); object_type=get_object_type(); if (AOT_ER_BSAC) ext_chan_config=get_bits(4) }
    Sync extension (sync_extension set, ext != SBR): while (get_bits_left>15) { if (show_bits(11)==0x2b7) { skip 11; ext_object_type=get_object_type(); if (ext==AOT_SBR && (sbr=get_bits1())==1) { ext_rate=get_sample_rate(); if (ext_rate==rate) sbr=-1 } if (get_bits_left>11 && get_bits(11)==0x548) ps=get_bits1(); break; } else get_bits1(); }
  use: HE-AAC v1 (SBR) and v2 (PS) flags, true output rate and channels.
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/mpeg4audio.c

- field: ac3_descriptive_bsi
  where: AC-3 bsi and E-AC-3 bsi (per frame; read from first frame)
  parse: |
    MediaInfo File_Ac3.cpp:
    dialnorm dB = -dialnorm, with dialnorm==0 mapped to -31.
    compr (only when compre set): AC3_compr[compr>>4] + 20*log10((0x10+(compr&0x0F))/32)
    dsurmod==2 -> "Dolby Surround"; dsurexmod==2 -> "Dolby Surround EX"; dsurexmod==3 -> "Dolby Pro Logic IIz"
    cmixlev dB = -3 - 1.5*cmixlev (only if <=2); surmixlev 2 -> "-inf", else -3 - 3*surmixlev
    dmixmod: AC3_dmixmod[dmixmod-1] if nonzero; bsmod -> service kind (AC3_Mode table)
  use: All useful descriptive tags.
  source: File_Ac3.cpp (lines ~1200-1212, 1398-1412, 1436, 1533-1571). Bit positions for dsurexmod and dmixmod were not read: UNVERIFIED.
```

bdinfo-rs-core:
```yaml
- field: bdinfo_rs_core
  where: crates.io bdinfo-rs-core 4.0.0 (static.crates.io .crate download, read locally)
  parse: |
    license LGPL-2.1-or-later. repository https://github.com/agentjp/bdinfo-rs. created 2026-06-19. 537 total downloads. rust 1.96, edition 2024.
    Scanners take BDInfo's TsAudioStream/TsStreamBuffer: not a header-level API.
    ac3.rs: EMDF 0x5838 scan and JOC payload id 14 (~lines 207-267). truehd.rs: has_extensions = Atmos. dts_hd.rs: DTS:X markers 0x02000850 and 0xF14000D0.
  use: Reference to port logic. LGPL: vendoring code needs LGPL terms. As a dependency it needs an adapter to BDInfo types. Not a standalone parser for MKV/MP4 frames.
```

## Dead ends
- MediaInfoLib File_Mlp.cpp: 404 at master. TrueHD/MLP parsing lives in File_Ac3.cpp (HD_StreamType 0xBA = TrueHD, 0xBB = MLP).
- DTS dialnorm: zero hits for "dialnorm" in File_Dts.cpp. DTS core has no dialnorm field in MediaInfo.
- FFmpeg dca_core.c: never sets an X profile.
- MP4 dec3 layout (ETSI TS 102 366 Annex F): not fetched. UNVERIFIED.
- MKV A_EAC3 CodecPrivate: not checked. Assume empty, so scan frames. UNVERIFIED.

## Leads
- File_Dts.cpp dtsx_bit_reader: type-241 presentation metadata for DTS:X object count.
- Test corpus for TrueHD num_substreams==4 vs MSB-only disagreement.
