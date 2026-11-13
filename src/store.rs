//! PostgreSQL metadata helpers and storage-independent catalog/message-ID facts.
//! No SQLite connection, shard discovery, schema or writer engine lives here.

pub(crate) use crate::catalog_facts::{
    ArticleFacts, ROUTE_SLACK_SECS, VIDEO_EXTENSIONS, adds_parts, due, has_part, insert_part, is_exactly, keyed_hash,
    merge_parts, one_video_named, parts_overlap, same_parts, subject_key,
};
pub use crate::catalog_facts::{
    COMPLETE_SETTLE_SECS, FileInfo, READ_MAX_SEGMENTS, SEAL_AGE, SEAL_MAX_SEGMENTS, SETTLE_SECS, SHARDS, SegmentPick,
    SegmentRef, global_id, seen_count, shard_of, shard_of_id,
};

pub fn meta(db: &dyn crate::pg::sql::Sql, key: &str) -> crate::pg::sql::Result<Option<i64>> {
    Ok(db.opt::<Option<i64>>("select value from meta where key = $1", crate::pg::sql::vals![key])?.flatten())
}

pub fn put_meta(db: &dyn crate::pg::sql::Sql, key: &str, value: i64) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into meta (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value",
        crate::pg::sql::vals![key, value],
    )?;
    Ok(())
}

pub fn meta_text(db: &dyn crate::pg::sql::Sql, key: &str) -> crate::pg::sql::Result<Option<String>> {
    Ok(db.opt::<Option<String>>("select text_value from meta where key = $1", crate::pg::sql::vals![key])?.flatten())
}

pub fn put_meta_text(db: &dyn crate::pg::sql::Sql, key: &str, value: &str) -> crate::pg::sql::Result<()> {
    db.execute("insert into meta (key, text_value) values ($1, $2) on conflict (key) do update set text_value = excluded.text_value",
        crate::pg::sql::vals![key, value])?;
    Ok(())
}

const TEXT: u8 = 0;
const HEX_LOWER: u8 = 1;
const HEX_UPPER: u8 = 2;

const DIGITS: u8 = 3;
const BASE36_LOWER: u8 = 4;
const BASE36_UPPER: u8 = 5;
const BASE62: u8 = 6;
const BASE64_URL: u8 = 7;

/// the alphabet of each packed tag, by digit value
fn alphabet(tag: u8) -> &'static [u8] {
    match tag {
        DIGITS => b"0123456789",
        BASE36_LOWER => b"0123456789abcdefghijklmnopqrstuvwxyz",
        BASE36_UPPER => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        BASE62 => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
        BASE64_URL => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_",
        _ => b"",
    }
}

/// digits of base `n` that fit one u32 limb step: the most `c` with `n^c <= 2^32`, and `n^c`
fn chunk(n: u32) -> (usize, u64) {
    let (mut c, mut m) = (0usize, 1u64);
    while m * u64::from(n) <= 1 << 32 {
        m *= u64::from(n);
        c += 1;
    }
    (c, m)
}

/// big-endian base-256 bytes of a base-`n` number given as digit values, no leading zero byte (none at all for 0)
fn to_bytes(digits: &[u8], n: u32) -> Vec<u8> {
    let (c, _) = chunk(n);
    // little-endian u32 limbs, a chunk of digits folded in at a time
    let mut limbs: Vec<u32> = Vec::with_capacity(digits.len() / 4 + 1);
    for group in digits.chunks(c) {
        let (mut m, mut v) = (1u64, 0u64);
        for &d in group {
            m *= u64::from(n);
            v = v * u64::from(n) + u64::from(d);
        }
        let mut carry = v;
        for limb in limbs.iter_mut() {
            let t = u64::from(*limb) * m + carry;
            *limb = t as u32;
            carry = t >> 32;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
    }
    let mut out = Vec::with_capacity(limbs.len() * 4);
    for limb in limbs.iter().rev() {
        out.extend_from_slice(&limb.to_be_bytes());
    }
    let zeros = out.iter().take_while(|b| **b == 0).count();
    out.drain(..zeros);
    out
}

/// `len` base-`n` digit values of a big-endian base-256 number (the lowest `len`, when it has more)
fn from_bytes(bytes: &[u8], n: u32, len: usize) -> Vec<u8> {
    let (c, m) = chunk(n);
    // big-endian u32 limbs
    let mut limbs: Vec<u32> = Vec::with_capacity(bytes.len() / 4 + 1);
    let head = bytes.len() % 4;
    if head > 0 {
        limbs.push(bytes[..head].iter().fold(0u32, |v, b| (v << 8) | u32::from(*b)));
    }
    limbs.extend(bytes[head..].chunks(4).map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]])));
    let mut digits = vec![0u8; len];
    let mut at = len;
    let mut start = 0;
    while at > 0 {
        while start < limbs.len() && limbs[start] == 0 {
            start += 1;
        }
        if start == limbs.len() {
            // the rest are leading zeros
            break;
        }
        let mut rem = 0u64;
        for limb in &mut limbs[start..] {
            let v = (rem << 32) | u64::from(*limb);
            *limb = (v / m) as u32;
            rem = v % m;
        }
        for _ in 0..c {
            if at == 0 {
                break;
            }
            at -= 1;
            digits[at] = (rem % u64::from(n)) as u8;
            rem /= u64::from(n);
        }
    }
    digits
}

/// Which characters each packed alphabet takes: bit 0 digits, 1 lowercase, 2 uppercase, 3 `-` and `_`, 4 anything
/// else
fn char_class(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => 1,
        b'a'..=b'z' => 2,
        b'A'..=b'Z' => 4,
        b'-' | b'_' => 8,
        _ => 16,
    }
}

/// a character's digit value in a packed alphabet it is in
fn digit_value(tag: u8, b: u8) -> u8 {
    match (tag, b) {
        (_, b'0'..=b'9') => b - b'0',
        (BASE36_LOWER, b'a'..=b'z') => b - b'a' + 10,
        (_, b'A'..=b'Z') => b - b'A' + 10,
        (_, b'a'..=b'z') => b - b'a' + 36,
        (_, b'-') => 62,
        _ => 63,
    }
}

/// Packs a message-id's local part by its shape: hex at half the size
/// (tags 1, 2), other single-alphabet ids as a base-N number (tags 3-7,
/// then `[len]`), anything else as text (tag 0). Deterministic, and
/// `unpack_local` gives back exactly the same string.
pub(crate) fn pack_local(local: &str) -> Vec<u8> {
    let bytes = local.as_bytes();
    let mut classes = 0u8;
    let mut hex_lower = true;
    let mut hex_upper = true;
    for &b in bytes {
        classes |= char_class(b);
        hex_lower &= b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        hex_upper &= b.is_ascii_digit() || (b'A'..=b'F').contains(&b);
    }
    let even = !bytes.is_empty() && bytes.len().is_multiple_of(2);
    if even && (hex_lower || hex_upper) {
        let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        let mut out = Vec::with_capacity(1 + bytes.len() / 2);
        out.push(if hex_lower { HEX_LOWER } else { HEX_UPPER });
        out.extend(bytes.chunks(2).map(|p| (nibble(p[0]) << 4) | nibble(p[1])));
        return out;
    }
    // the first alphabet, in the order DIGITS, BASE36_LOWER, BASE36_UPPER, BASE62, BASE64_URL, with every character
    let tag = match classes {
        _ if !(1..=255).contains(&bytes.len()) || classes & 16 != 0 => TEXT,
        1 => DIGITS,
        c if c & 8 != 0 => BASE64_URL,
        c if c & 6 == 6 => BASE62,
        c if c & 2 != 0 => BASE36_LOWER,
        _ => BASE36_UPPER,
    };
    if tag == TEXT {
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(TEXT);
        out.extend_from_slice(bytes);
        return out;
    }
    let values: Vec<u8> = bytes.iter().map(|&b| digit_value(tag, b)).collect();
    let mut out = vec![tag, bytes.len() as u8];
    out.extend(to_bytes(&values, alphabet(tag).len() as u32));
    out
}

/// A local part back from any of the packings (tags 0-7).
pub(crate) fn unpack_local(blob: &[u8]) -> String {
    const LOWER: &[u8; 16] = b"0123456789abcdef";
    const UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let hex = |digits: &[u8; 16]| -> String {
        let mut out = String::with_capacity(2 * blob.len());
        for b in &blob[1..] {
            out.push(digits[usize::from(b >> 4)] as char);
            out.push(digits[usize::from(b & 15)] as char);
        }
        out
    };
    match blob.first() {
        Some(&HEX_LOWER) => hex(LOWER),
        Some(&HEX_UPPER) => hex(UPPER),
        Some(&tag) if (DIGITS..=BASE64_URL).contains(&tag) && blob.len() >= 2 => {
            let abc = alphabet(tag);
            from_bytes(&blob[2..], abc.len() as u32, blob[1] as usize)
                .iter()
                .map(|&d| abc.get(usize::from(d)).copied().unwrap_or(b'0') as char)
                .collect()
        }
        _ => String::from_utf8_lossy(blob.get(1..).unwrap_or_default()).into_owned(),
    }
}

/// a local packed the current way, whatever way it was packed before
pub(crate) fn repack(local: &[u8]) -> Vec<u8> {
    pack_local(&unpack_local(local))
}

/// `<local@domain>` -> (local, "@domain>"); anything else stays whole
pub(crate) fn split_message_id(id: &str) -> Option<(&str, &str)> {
    let inner = id.strip_prefix('<')?;
    if !id.ends_with('>') {
        return None;
    }
    let at = inner.rfind('@')?;
    Some((&inner[..at], &inner[at..]))
}

pub(crate) fn whole(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + id.len());
    out.push(TEXT);
    out.extend_from_slice(id.as_bytes());
    out
}

pub(crate) fn dedup_key(local: &[u8], domain: i64, suffix: Option<&str>) -> String {
    match (domain, suffix) {
        (0, _) | (_, Some(_)) => decode(local, suffix),
        _ => format!("\0{domain}\0{}", local.iter().map(|b| format!("{b:02x}")).collect::<String>()),
    }
}

pub(crate) fn decode(local: &[u8], suffix: Option<&str>) -> String {
    match suffix {
        None => unpack_local(local),
        Some(suffix) => format!("<{}{suffix}", unpack_local(local)),
    }
}

/// Neutral keyed-hash input retained for metadata renderers. No connection,
/// storage identity or write authority is created by this value.
pub(crate) struct IdKey {
    k0: u64,
    k1: u64,
}
impl IdKey {
    pub(crate) fn hash(&self, id: &str) -> i64 {
        keyed_hash(self.k0, self.k1, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod first_packing {
        use super::super::{BASE64_URL, DIGITS, HEX_LOWER, HEX_UPPER, TEXT, alphabet};

        fn to_bytes(digits: &[u8], n: u32) -> Vec<u8> {
            let mut out: Vec<u8> = Vec::new();
            for &d in digits {
                let mut carry = u32::from(d);
                for b in out.iter_mut() {
                    let v = u32::from(*b) * n + carry;
                    *b = (v & 0xff) as u8;
                    carry = v >> 8;
                }
                while carry > 0 {
                    out.push((carry & 0xff) as u8);
                    carry >>= 8;
                }
            }
            out.reverse();
            out
        }

        fn from_bytes(bytes: &[u8], n: u32, len: usize) -> Vec<u8> {
            let mut num: Vec<u8> = bytes.to_vec();
            let mut digits = Vec::with_capacity(len);
            for _ in 0..len {
                let mut rem = 0u32;
                for b in num.iter_mut() {
                    let v = (rem << 8) | u32::from(*b);
                    *b = (v / n) as u8;
                    rem = v % n;
                }
                digits.push(rem as u8);
            }
            digits.reverse();
            digits
        }

        pub fn pack(local: &str) -> Vec<u8> {
            let bytes = local.as_bytes();
            let even = !bytes.is_empty() && bytes.len().is_multiple_of(2);
            let lower = even && bytes.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
            let upper = even && !lower && bytes.iter().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b));
            if lower || upper {
                let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                let mut out = vec![if lower { HEX_LOWER } else { HEX_UPPER }];
                out.extend(bytes.chunks(2).map(|p| (nibble(p[0]) << 4) | nibble(p[1])));
                return out;
            }
            if (1..=255).contains(&bytes.len()) {
                for tag in DIGITS..=BASE64_URL {
                    let abc = alphabet(tag);
                    let values: Option<Vec<u8>> =
                        bytes.iter().map(|b| abc.iter().position(|a| a == b).map(|p| p as u8)).collect();
                    if let Some(values) = values {
                        let mut out = vec![tag, bytes.len() as u8];
                        out.extend(to_bytes(&values, abc.len() as u32));
                        return out;
                    }
                }
            }
            [&[TEXT][..], bytes].concat()
        }

        pub fn unpack(blob: &[u8]) -> String {
            match blob.first() {
                Some(&HEX_LOWER) => blob[1..].iter().map(|b| format!("{b:02x}")).collect(),
                Some(&HEX_UPPER) => blob[1..].iter().map(|b| format!("{b:02X}")).collect(),
                Some(&tag) if (DIGITS..=BASE64_URL).contains(&tag) && blob.len() >= 2 => {
                    let abc = alphabet(tag);
                    from_bytes(&blob[2..], abc.len() as u32, blob[1] as usize)
                        .iter()
                        .map(|&d| abc[d as usize] as char)
                        .collect()
                }
                _ => String::from_utf8_lossy(blob.get(1..).unwrap_or_default()).into_owned(),
            }
        }
    }

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL metadata helper qualification"]
    fn native_meta_keeps_numbers_texts_and_driver_errors_distinct() {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let mut client = schema.pool.get().unwrap();
        {
            let pg = crate::pg::sql::Pg::new(&mut client);
            assert_eq!(meta_text(&pg, "owner").unwrap(), None);
            put_meta_text(&pg, "owner", "a.example").unwrap();
            put_meta_text(&pg, "owner", "b.example").unwrap();
            assert_eq!(meta_text(&pg, "owner").unwrap().as_deref(), Some("b.example"));
            assert_eq!(meta(&pg, "owner").unwrap(), None, "text is not coerced into an integer");
            put_meta(&pg, "count", 7).unwrap();
            assert_eq!(meta(&pg, "count").unwrap(), Some(7));
            put_meta(&pg, "count", i64::MIN).unwrap();
            assert_eq!(meta(&pg, "count").unwrap(), Some(i64::MIN));
            assert_eq!(meta_text(&pg, "count").unwrap(), None);
            assert_eq!(meta(&pg, "next_seq").unwrap(), Some(1));
        }
        client.batch_execute("alter table meta rename to unavailable_metadata").unwrap();
        let pg = crate::pg::sql::Pg::new(&mut client);
        let error = meta(&pg, "count").unwrap_err();
        assert_eq!(
            error.postgres().unwrap().code(),
            Some(&postgres::error::SqlState::UNDEFINED_TABLE),
            "missing storage must retain its original driver cause"
        );
    }

    #[test]
    fn locals_pack_by_shape_and_come_back_exactly() {
        let cases = [
            ("1064b678f3f54e28a5afd48a3a986076", 1u8), // ngPost hex
            ("ABCDEF0123", 2),
            ("0001234", 3), // digits (odd length, so not hex), leading zeros kept
            ("0abc9xyz", 4),
            ("0ABC9XYZ", 5),
            ("hotfTpetaZRIbOYuTuQ31", 6), // JBinUp
            ("DR59tDkIGMDKQS1YflogRq2MTqVgoHslO", 6),
            ("ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235", 6), // Nyuu: letters + ms timestamp
            ("a-b_c-9", 7),
            ("abc", 4),                   // odd length hex-looking goes base36
            ("nnd$009a5634$43be2fd7", 0), // anything else stays text
            ("", 0),
        ];
        for (local, tag) in cases {
            let packed = pack_local(local);
            assert_eq!(packed[0], tag, "{local}");
            assert_eq!(unpack_local(&packed), local, "{local}");
            assert_eq!(pack_local(local), packed, "deterministic: {local}");
        }
        let long = "z".repeat(300); // not "a": that would be hex
        assert_eq!(pack_local(&long)[0], 0, "longer than 255 stays text");
        assert_eq!(pack_local("ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235").len(), 30, "base62 is smaller than text (38)");
    }

    #[test]
    fn packing_is_byte_for_byte_what_it_always_was() {
        // a small deterministic generator: ids of every alphabet and length, and the odd character out
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let pools: [&[u8]; 8] = [
            b"0123456789",
            b"0123456789abcdef",
            b"0123456789ABCDEF",
            b"0123456789abcdefghijklmnopqrstuvwxyz",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_",
            b"0123456789abcXYZ-_.$%+=",
        ];
        let mut cases: Vec<String> = vec![String::new(), "0".into(), "00".into(), "000".into(), "0000000a".into()];
        for len in [1usize, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 40, 64, 100, 254, 255, 256, 300] {
            for pool in pools {
                for _ in 0..20 {
                    cases.push((0..len).map(|_| pool[(next() % pool.len() as u64) as usize] as char).collect());
                }
                // leading zero digits
                cases.push(format!(
                    "00{}",
                    (0..len).map(|_| pool[(next() % pool.len() as u64) as usize] as char).collect::<String>()
                ));
            }
        }
        for local in &cases {
            let packed = pack_local(local);
            assert_eq!(packed, first_packing::pack(local), "packs {local:?} as before");
            assert_eq!(unpack_local(&packed), *local, "round trip of {local:?}");
            assert_eq!(unpack_local(&packed), first_packing::unpack(&packed));
        }
        // blobs no packing makes (a stored length that doesnt match the number) still decode as before
        for tag in DIGITS..=BASE64_URL {
            for blob in [vec![tag, 0], vec![tag, 3, 0xff, 0xff, 0xff, 0xff], vec![tag, 40, 1, 2], vec![tag, 1, 0, 0, 7]]
            {
                assert_eq!(unpack_local(&blob), first_packing::unpack(&blob), "{blob:?}");
            }
        }
    }

    #[test]
    fn old_packings_still_decode_and_repack_to_the_current_one() {
        // text and hex from before tags 3-7 keep their meaning
        assert_eq!(unpack_local(&[TEXT, b'h', b'i']), "hi");
        assert_eq!(unpack_local(&[HEX_LOWER, 0xab, 0x01]), "ab01");
        assert_eq!(unpack_local(&[HEX_UPPER, 0xab, 0x01]), "AB01");
        let old_text = [&[TEXT][..], b"hotfTpetaZRIbOYuTuQ31"].concat();
        let current = pack_local("hotfTpetaZRIbOYuTuQ31");
        assert_eq!(repack(&old_text), current);
        assert_eq!(repack(&current), current, "repacking is stable");
        assert_eq!(repack(&[HEX_LOWER, 0xab]), vec![HEX_LOWER, 0xab]);
        // the longest ids still round-trip
        let max = "z".repeat(255);
        assert_eq!(unpack_local(&pack_local(&max)), max);
        let max64 = format!("{}-", "Z".repeat(254));
        assert_eq!(unpack_local(&pack_local(&max64)), max64);
    }

    #[test]
    fn ids_say_their_shard_and_keep_order() {
        let ids: Vec<i64> = (0..SHARDS).map(|s| global_id(10, s)).collect();
        for (s, id) in ids.iter().enumerate() {
            assert_eq!(shard_of_id(*id), s);
        }
        assert!(global_id(11, 0) > global_id(10, SHARDS - 1), "a later seq sorts after, whatever the shard");
    }

    #[test]
    fn message_id_split_and_whole_codecs_preserve_exact_source_bytes() {
        for id in ["<abc@def@ghi>", "<0001@domain>", "<@empty-local>", "no-brackets@x", "<no-at-sign>", ""] {
            assert_eq!(decode(&whole(id), None), id);
            if let Some((local, suffix)) = split_message_id(id) {
                assert_eq!(decode(&pack_local(local), Some(suffix)), id);
            }
        }
        assert_eq!(dedup_key(&[TEXT, b'a'], 7, None), "\u{0}7\u{0}0061");
    }
}
