//! Admission of the existing complete kept ProbeInfo representation. Checking
//! only JSON bytes misses arrays of tiny objects with large defaulted structs.
//! The first pass allocates no JSON tree and admits every structural node and
//! decoded string before the existing typed deserializer is allowed to run.
use anyhow::{Context, Result, ensure};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

struct Shape { left: u64, node: u64 }
impl Shape {
    fn charge<E: de::Error>(&mut self, bytes: u64) -> std::result::Result<(), E> {
        self.left = self.left.checked_sub(bytes).ok_or_else(|| E::custom("kept probe exceeds owned-view admission"))?;
        Ok(())
    }
}
struct Scan<'a>(&'a mut Shape);
impl<'de> DeserializeSeed<'de> for Scan<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        self.0.charge::<D::Error>(self.0.node)?;
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Scan<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("kept probe JSON") }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> { Ok(()) }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> { Ok(()) }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> { Ok(()) }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> { Ok(()) }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> { Ok(()) }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<(), E> {
        // Covers decoded ownership, JSON escape scratch, and the fact
        // renderer's escaped/joined strings while the typed value is retained.
        self.0.charge::<E>((value.len() as u64).checked_mul(64).ok_or_else(|| E::custom("probe text overflow"))?)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> std::result::Result<(), A::Error> {
        while values.next_element_seed(Scan(&mut *self.0))?.is_some() {} Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> std::result::Result<(), A::Error> {
        while values.next_key_seed(Scan(&mut *self.0))?.is_some() {
            values.next_value_seed(Scan(&mut *self.0))?;
        }
        Ok(())
    }
}

pub(crate) fn decode(bytes: &[u8], remaining: &mut u64) -> Result<crate::probe::ProbeInfo> {
    use std::mem::size_of;
    use crate::probe::{ProbeInfo, archive::ArchiveHead, media::*, nest::Layer, yenc::YencLines, yenc_quirks::{Quirks, Escapes, Votes}};
    // All types reachable by ProbeInfo's derived Deserialize. Transient cover
    // bytes/track_uids/crypt parameters are serde(skip), as in legacy storage.
    // Four element widths cover Vec growth/reallocation; 512 also reserves
    // map buckets/tree nodes. Charge keys and ignored fields too, conservatively.
    let width = [size_of::<ProbeInfo>(), size_of::<ArchiveHead>(), size_of::<MediaInfo>(),
        size_of::<VideoTrack>(), size_of::<AudioTrack>(), size_of::<SubtitleTrack>(),
        size_of::<TrackStats>(), size_of::<Attachment>(), size_of::<Chapter>(),
        size_of::<DolbyVisionInfo>(), size_of::<Layer>(), size_of::<YencLines>(),
        size_of::<Quirks>(), size_of::<Escapes>(), size_of::<Votes>(),
        size_of::<(String, String)>()].into_iter().max().unwrap() as u64;
    let scratch = (bytes.len() as u64).checked_mul(2).context("probe scratch overflow")?;
    let mut shape = Shape { left: remaining.checked_sub(scratch).context("probe JSON scratch exceeds admission")?,
        node: width.checked_mul(4).and_then(|n| n.checked_add(512)).context("probe node admission overflow")? };
    let mut json = serde_json::Deserializer::from_slice(bytes);
    Scan(&mut shape).deserialize(&mut json)?;
    json.end()?;
    let probe: ProbeInfo = serde_json::from_slice(bytes).context("kept probe shape is invalid")?;
    ensure!(shape.left <= *remaining, "probe admission accounting overflow");
    *remaining = shape.left;
    Ok(probe)
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_kept_probe_roundtrip_and_default_struct_array_admission() {
        let mut probe = crate::probe::ProbeInfo::default();
        probe.nfo = Some("<&é>".into());
        probe.headers.insert("Injection-Info".into(), "private account".into());
        probe.media = Some(crate::probe::media::MediaInfo { video: vec![Default::default(); 2], ..Default::default() });
        let bytes = serde_json::to_vec(&probe).unwrap();
        assert_eq!(decode(&bytes, &mut (4 * 1024 * 1024)).unwrap(), probe);
        let mut budget = bytes.len() as u64 * 2 + 64;
        assert!(decode(&bytes, &mut budget).is_err());
        assert!(decode(b"{}{}", &mut (4 * 1024 * 1024)).is_err());
        assert!(decode(b"{\"media\":{\"video\":[{},{}]}}", &mut 1024).is_err());
    }
}
