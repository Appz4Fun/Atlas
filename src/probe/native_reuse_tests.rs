//! Native cache admission controls. Source only until the controlled runner executes them.
use super::*;

fn current() -> ProbeInfo {
    ProbeInfo {
        file: Some("video.mkv".into()),
        yenc: Some(yenc::YencLines { file_size: Some(8192), ..Default::default() }),
        file_crc32: Some(0x12345678),
        moviehash: Some("0011223344556677".into()),
        media_whole: Some(whole_mark(127)),
        media: Some(media::MediaInfo {
            container: "matroska".into(),
            segment_uid: Some("00112233445566778899aabbccddeeff".into()),
            muxed_at: Some(1700000000),
            header_complete: true, encoder_complete: true,
            attachments_complete: true, chapters_complete: true,
            video: vec![media::VideoTrack { codec: "V_AV1".into(), ..Default::default() }],
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn json(probe: &ProbeInfo) -> String { serde_json::to_string(probe).unwrap() }

#[test]
fn native_saved_probe_distinguishes_missing_coverage_from_partial_and_known_absence() {
    let p=current(); let original=json(&p);
    assert!(matches!(native_saved_probe(&original,&mut (32<<20)).unwrap(),SavedCoverage::Current(_)));
    let mut old=serde_json::to_value(&p).unwrap();
    old["media"].as_object_mut().unwrap().remove("header_complete");
    let old=old.to_string(); let preserved=old.clone();
    assert!(matches!(native_saved_probe(&old,&mut (32<<20)).unwrap(),SavedCoverage::NeedsTargetRefresh));
    assert_eq!(old,preserved,"admission never rewrites preserved evidence");
    let mut partial=p.clone(); partial.media.as_mut().unwrap().header_complete=false;
    assert!(matches!(native_saved_probe(&json(&partial),&mut (32<<20)).unwrap(),SavedCoverage::Current(_)),"explicit partial is not a fake missing row");
    let mut absent=p; absent.media.as_mut().unwrap().subtitles.push(Default::default());
    assert!(matches!(native_saved_probe(&json(&absent),&mut (32<<20)).unwrap(),SavedCoverage::Current(_)),"explicit null header key is known absence");
    let mut old=serde_json::to_value(&absent).unwrap();
    old["media"]["subtitles"][0].as_object_mut().unwrap().remove("header_key");
    assert!(matches!(native_saved_probe(&old.to_string(),&mut (32<<20)).unwrap(),SavedCoverage::NeedsTargetRefresh));
}

#[test]
fn native_saved_probe_rejects_malformed_or_expanded_shape_instead_of_missing() {
    assert!(native_saved_probe("{",&mut (32<<20)).is_err());
    let mut expanded=current(); expanded.media.as_mut().unwrap().video=vec![Default::default();1000];
    let expanded=json(&expanded);
    assert!(native_saved_probe(&expanded,&mut (64<<10)).is_err(),"tiny defaulted track objects require owned allocation admission");
    assert!(native_saved_probe(&json(&current()),&mut 1).is_err());
}

fn candidate(own:&ProbeInfo,other:&ProbeInfo)->anyhow::Result<Option<SameMux>> {
    native_same_mux_candidate(1,2,"alt.binaries.example",own,&json(other),other.moviehash.as_deref(),None,127,&mut (32<<20))
}

#[test]
fn native_same_mux_reuses_current_checked_nonasset_media_only() {
    let own=current(); let other=current();
    let got=candidate(&own,&other).unwrap().expect("genuine matching current media");
    assert_eq!(got.release_id,2); assert_eq!(got.media,other.media.unwrap()); assert!(got.cover.is_none());
}

#[test]
fn native_same_mux_rejects_wrong_or_missing_file_evidence_and_stale_coverage() {
    let own=current();
    for change in 0..8 {
        let mut other=current();
        match change {
            0=>other.file_crc32=None,
            1=>other.file_crc32=Some(0x87654321),
            2=>other.moviehash=Some("ffeeddccbbaa9988".into()),
            3=>other.yenc.as_mut().unwrap().file_size=Some(8193),
            4=>other.media.as_mut().unwrap().segment_uid=Some("other mux".into()),
            5=>other.media.as_mut().unwrap().title=Some("different header".into()),
            6=>other.media_whole=Some(whole_mark(63)),
            _=>other.media.as_mut().unwrap().header_complete=false,
        }
        assert!(candidate(&own,&other).unwrap().is_none(),"mismatch {change}");
    }
    let mut old=serde_json::to_value(current()).unwrap(); old["media"].as_object_mut().unwrap().remove("header_complete");
    assert!(native_same_mux_candidate(1,2,"alt.binaries.example",&own,&old.to_string(),own.moviehash.as_deref(),None,127,&mut (32<<20)).unwrap().is_none());
    assert!(native_same_mux_candidate(1,1,"alt.binaries.example",&own,&json(&own),own.moviehash.as_deref(),None,127,&mut (32<<20)).unwrap().is_none());
    assert!(native_same_mux_candidate(1,2,&"x".repeat(513),&own,&json(&own),own.moviehash.as_deref(),None,127,&mut (32<<20)).unwrap().is_none(),"blocked group cannot lend");
}

#[test]
fn native_same_mux_refuses_unqualified_images_and_does_not_swallow_decode_errors() {
    let own=current(); let mut other=current();
    other.media.as_mut().unwrap().attachments.push(media::Attachment {name:"cover.jpg".into(),mime:Some("image/jpeg".into()),..Default::default()});
    assert!(candidate(&own,&other).unwrap().is_none(),"exists() is not an asset grant");
    assert!(native_same_mux_candidate(1,2,"alt.binaries.example",&own,"{",own.moviehash.as_deref(),None,127,&mut (32<<20)).is_err());
}

#[test]
fn native_same_mux_does_not_promote_known_partial_attachments_or_chapters() {
    let mut own=current(); own.media.as_mut().unwrap().header_complete=false;
    let mut other=current();
    other.media.as_mut().unwrap().attachments.push(media::Attachment {name:"font.ttf".into(),..Default::default()});
    other.media.as_mut().unwrap().attachments_complete=false;
    assert!(candidate(&own,&other).unwrap().is_none(),"capped attachment evidence cannot become whole by metadata reuse");
    other=current(); other.media.as_mut().unwrap().chapters.push(Default::default());
    other.media.as_mut().unwrap().chapters_complete=false;
    assert!(candidate(&own,&other).unwrap().is_none(),"a complete header is not complete chapter-element evidence");
}

#[test]
fn native_same_mux_rejects_real_parser_covers_without_relying_on_declared_mime() {
    fn el(id:&[u8],body:&[u8])->Vec<u8> {
        assert!(body.len()<0x3fff);
        [id,&[0x40|((body.len()>>8)as u8),body.len()as u8],body].concat()
    }
    let jpeg=[&[0xff,0xd8,0xff,0xe0][..],&[7;100],&[0xff,0xd9]].concat();
    for mime in [None,Some("application/octet-stream"),Some("Image/JPEG")] {
        let mut fields=el(&[0x46,0x6e],b"cover.jpg");
        if let Some(mime)=mime {fields.extend(el(&[0x46,0x60],mime.as_bytes()));}
        fields.extend(el(&[0x46,0x5c],&jpeg));
        let attachments=el(&[0x19,0x41,0xa4,0x69],&el(&[0x61,0xa7],&fields));
        let segment=[attachments,el(&[0x1f,0x43,0xb6,0x75],&[])].concat();
        let bytes=[el(&[0x1a,0x45,0xdf,0xa3],&el(&[0x42,0x82],b"matroska")),el(&[0x18,0x53,0x80,0x67],&segment)].concat();
        let parsed=media::parse(&bytes).unwrap();
        assert_eq!(parsed.cover.as_ref().unwrap().bytes,jpeg,"real producer recognizes JPEG despite {mime:?}");
        let mut other=current();
        other.media.as_mut().unwrap().attachments=parsed.attachments;
        other.media.as_mut().unwrap().cover=parsed.cover;
        let stored=json(&other);
        let decoded:ProbeInfo=serde_json::from_str(&stored).unwrap();
        assert!(decoded.media.as_ref().unwrap().cover.is_none(),"serde intentionally drops attachment payload");
        let mut own=current();own.media.as_mut().unwrap().header_complete=false;
        assert!(candidate(&own,&decoded).unwrap().is_none(),"filename-recognized image needs an actual asset grant: {mime:?}");
    }
}

#[test]
fn native_same_mux_refuses_existing_cover_descriptor_even_without_attachment_metadata() {
    let own=current();let other=current();
    assert!(native_same_mux_candidate(1,2,"alt.binaries.example",&own,&json(&other),other.moviehash.as_deref(),Some(("image/jpeg","retained")),127,&mut (32<<20)).unwrap().is_none());
    assert!(!native_media_needs_asset(own.media.as_ref()));
    let mut image=other;image.media.as_mut().unwrap().attachments.push(media::Attachment {name:"opaque".into(),mime:Some("Image/JPEG".into()),..Default::default()});
    assert!(native_media_needs_asset(image.media.as_ref()),"same conservative policy applies to own saved probes");
}
