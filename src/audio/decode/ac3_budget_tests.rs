//! Real syncframes in one admitted Matroska block must obey the sound budget.
use super::{AudioTrack, decode};
use crate::audio::mkv;

const FIXTURES: [(&str, &[u8]); 2] = [
    ("AC-3", include_bytes!("../../../tests/fixtures/audio/tune.ac3.mkv")),
    ("E-AC-3", include_bytes!("../../../tests/fixtures/audio/tune.eac3.mkv")),
];

fn fixture(data: &[u8]) -> (AudioTrack, Vec<u8>, usize, u32) {
    let track = mkv::audio_track(data).expect("tracked fixture has an audio track");
    let frames = mkv::frames(data, &track);
    let packet = &frames.first().expect("tracked fixture has a Matroska frame").bytes;
    let header = filmcraft_ac3::parse_header(packet).expect("tracked fixture has a syncframe");
    let syncframe = packet[..header.frame_bytes].to_vec();
    let mut decoder = filmcraft_ac3::Decoder::new();
    let decoded = decoder.decode(&syncframe).expect("tracked syncframe decodes");
    let samples = decoded.channels.first().expect("independent audio substream").len();
    assert!(samples > 0);
    assert_eq!(track.sample_rate, Some(f64::from(header.sample_rate)));
    (track, syncframe, samples, header.sample_rate)
}

fn block(track: &AudioTrack, syncframe: &[u8], count: usize) -> Vec<u8> {
    let bytes = syncframe.len().checked_mul(count).unwrap();
    assert!(count > 0 && bytes <= 4 * 1024 * 1024, "bounded input fits one production frame");
    let payload = syncframe.repeat(count);
    let number = u8::try_from(track.number).unwrap();
    assert!((1..128).contains(&number));
    let data = mkv::tests::cluster(0, &[mkv::tests::simple(number, 0, 0, &payload)]);
    let mut frames = mkv::frames(&data, track);
    assert_eq!(frames.len(), 1, "the production parser admits one crowded block");
    assert_eq!(frames[0].bytes, payload);
    frames.pop().unwrap().bytes
}

#[test]
fn one_block_caps_the_requested_duration_inside_its_syncframes() {
    for (name, data) in FIXTURES {
        let (track, syncframe, samples, rate) = fixture(data);
        let wanted = rate as usize * 35 / 1000;
        let crowded = block(&track, &syncframe, wanted.div_ceil(samples) + 2);
        let (actual_rate, decoded) = decode(&track, &[&crowded], 35).expect(name);
        assert_eq!(actual_rate, rate, "{name}");
        assert_eq!(decoded.iter().map(Vec::len).collect::<Vec<_>>(), [wanted], "{name}");
    }
}

#[test]
fn a_later_block_uses_only_the_budget_left_by_the_first_block() {
    for (name, data) in FIXTURES {
        let (track, syncframe, samples, rate) = fixture(data);
        let wanted = rate as usize * 75 / 1000;
        assert!(wanted > samples && wanted % samples != 0, "fixture crosses a partial syncframe: {name}");
        let crowded = block(&track, &syncframe, wanted.div_ceil(samples) + 2);
        let (_, decoded) = decode(&track, &[&syncframe, &crowded], 75).expect(name);
        assert_eq!(decoded.iter().map(Vec::len).collect::<Vec<_>>(), [samples, wanted - samples], "{name}");
    }
}

#[test]
fn one_block_cannot_exceed_the_hard_minute_even_for_an_unlimited_request() {
    for (name, data) in FIXTURES {
        let (track, syncframe, samples, rate) = fixture(data);
        let wanted = rate as usize * 60;
        let crowded = block(&track, &syncframe, wanted.div_ceil(samples) + 2);
        for requested_ms in [60_000, i64::MAX] {
            let (_, decoded) = decode(&track, &[&crowded], requested_ms).expect(name);
            assert_eq!(decoded.iter().map(Vec::len).collect::<Vec<_>>(), [wanted], "{name}, {requested_ms}");
        }
    }
}

#[test]
fn zero_duration_never_retains_samples() {
    for (name, data) in FIXTURES {
        let (track, syncframe, _, _) = fixture(data);
        let kept = decode(&track, &[&syncframe], 0)
            .map_or(0, |(_, decoded)| decoded.iter().map(Vec::len).sum::<usize>());
        assert_eq!(kept, 0, "{name}");
    }
}

#[test]
fn an_exhausted_budget_does_not_decode_a_later_broken_syncframe() {
    for (name, data) in FIXTURES {
        let (track, mut syncframe, _, rate) = fixture(data);
        syncframe.extend([0x0b, 0x77]);
        let (_, decoded) = decode(&track, &[&syncframe], 1).expect(name);
        assert_eq!(decoded.iter().map(Vec::len).collect::<Vec<_>>(), [rate as usize / 1000], "{name}");
    }
}

#[test]
fn short_blocks_keep_their_whole_syncframes() {
    for (name, data) in FIXTURES {
        let (track, syncframe, samples, rate) = fixture(data);
        let first = block(&track, &syncframe, 2);
        let (actual_rate, decoded) = decode(&track, &[&first, &syncframe], 1_000).expect(name);
        assert_eq!(actual_rate, rate, "{name}");
        assert_eq!(decoded.iter().map(Vec::len).collect::<Vec<_>>(), [samples * 2, samples], "{name}");
    }
}

#[test]
fn a_partial_final_frame_keeps_the_original_first_samples() {
    for (name,data) in FIXTURES {
        let (track,syncframe,samples,rate)=fixture(data);
        let crowded=block(&track,&syncframe,4);
        let (_,whole)=decode(&track,&[&crowded],1_000).expect(name);
        assert_eq!(whole[0].len(),samples*4);
        let wanted=rate as usize/1000;
        let (_,prefix)=decode(&track,&[&crowded],1).expect(name);
        assert_eq!(prefix,vec![whole[0][..wanted].to_vec()],"{name}: preserve the beginning, not a later slice");
        assert!(whole[0].iter().any(|sample|*sample!=0),"{name}: fixture must contain audible samples");
    }
}

#[test]
fn an_exact_syncframe_boundary_keeps_one_frame_and_skips_the_unneeded_broken_tail() {
    for (name,data) in FIXTURES {
        let (track,mut syncframe,samples,rate)=fixture(data);
        assert_eq!(samples*1000%rate as usize,0,"{name}: fixture has an exact millisecond boundary");
        let ms=(samples*1000/rate as usize) as i64;
        let (_,whole)=decode(&track,&[&syncframe],1_000).expect(name);
        syncframe.extend([0x0b,0x77]);
        let (_,bounded)=decode(&track,&[&syncframe],ms).expect(name);
        assert_eq!(bounded,whole,"{name}: an already satisfied frame must survive unread invalid data");
        let (_,needed)=decode(&track,&[&syncframe],ms+1).expect(name);
        assert_eq!(needed,vec![Vec::<i16>::new()],"{name}: a still-needed invalid syncframe invalidates its block");
    }
}

#[test]
fn a_dependent_substream_adds_no_samples_or_rate_before_the_primary_budget() {
    // E-AC-3, stream type1, id0,128bytes,48kHz,6blocks,stereo,bsid16.
    // Header-only dependent substreams decode to no channels by filmcraft's contract.
    let mut dependent=vec![0u8;128];dependent[..6].copy_from_slice(&[0x0b,0x77,0x40,0x3f,0x34,0x80]);
    let header=filmcraft_ac3::parse_header(&dependent).expect("dependent header");
    assert!(!header.is_primary());assert!(filmcraft_ac3::Decoder::new().decode(&dependent).unwrap().channels.is_empty());
    for (name,data) in FIXTURES {
        let (track,syncframe,_,rate)=fixture(data);
        let mut crowded=dependent.repeat(32);crowded.extend(syncframe.repeat(4));
        let (_,whole)=decode(&track,&[&syncframe],1_000).expect(name);
        let (actual,bounded)=decode(&track,&[&crowded],1).expect(name);
        assert_eq!(actual,rate,"{name}: dependent metadata must not establish the rate");
        assert_eq!(bounded,vec![whole[0][..rate as usize/1000].to_vec()],"{name}");
    }
}

#[test]
fn the_downmix_admits_only_the_requested_prefix_for_mono_and_multichannel_sound() {
    let channel=|value|vec![value;1536];
    for (channels,acmod,lfeon,want) in [
        (vec![channel(0.5)],1,false,16_384),
        (vec![channel(0.5),channel(-0.5)],2,false,0),
        (vec![channel(0.0),channel(0.0),channel(0.5),channel(1.0),channel(0.0),channel(0.0)],7,true,11_585),
    ] {
        assert_eq!(super::downmix(&channels,acmod,lfeon,3),vec![want;3]);
        assert!(super::downmix(&channels,acmod,lfeon,0).is_empty());
        assert_eq!(super::downmix(&channels,acmod,lfeon,1536),vec![want;1536]);
    }
    assert_eq!(super::downmix(&[vec![0.5;10],vec![0.5;2]],2,false,7),vec![16_384;2],"shortest channel still bounds the mix");
}
