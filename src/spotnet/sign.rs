//! Checks a spot's signatures the way Spotweb does, so a spot or a moderator's command that anyone could have
//! posted under another's name isn't taken.
//!
//! A spot's `From` is `Nickname <USERKEY.USERSIG@<header>.<HEADERSIG>>`. The header signature is RSA (PKCS #1
//! v1.5, SHA-1) over the title, the header up to its signature and the nickname. Keys 2 to 4 are Spotnet's own
//! (2 the moderators'), signed by its signing server; key 7 is a spot the spotter signed with their own key, which
//! must carry a hashcash (the SHA-1 of its message-id starts with four zero hex digits) to cost its poster some
//! work. A `From` that carries the spotter's key and signature must also have the message-id signed with that key:
//! that's what makes the spotter id theirs.

use num_bigint::BigUint;
use sha1::{Digest, Sha1};

use super::header::{decode_base64, spotter_id, unprepare_base64};

/// Spotnet's public keys by key id (moduli, base64; the exponent is 65537): the ones Spotweb ships.
const SPOTNET_KEYS: [(i32, &str); 3] = [
    (2, "ys8WSlqonQMWT8ubG0tAA2Q07P36E+CJmb875wSR1XH7IFhEi0CCwlUzNqBFhC+P"),
    (3, "uiyChPV23eguLAJNttC/o0nAsxXgdjtvUvidV2JL+hjNzc4Tc/PPo2JdYvsqUsat"),
    (4, "1k6RNDVD6yBYWR6kHmwzmSud7JkNV4SMigBrs+jFgOK5Ldzwl17mKXJhl+su/GR9"),
];

/// The key id of a spot the spotter signed with their own key.
pub const SELF_SIGNED_KEY: i32 = 7;

/// A restored, padded user key in a key-2 `From` longer than this makes it a
/// spotter's own `DISPOSE` of their spot, not a moderator's command.
const PERSONAL_KEY_LEN: usize = 50;

fn personal_key_length(prepared_len: usize, restored_len: usize) -> bool {
    // Spotweb pads BEFORE restoring '-s'/'-p'. Compare without allocating the
    // padding or adding it to a potentially maximal input byte length.
    let padding = (4 - prepared_len % 4) % 4;
    restored_len > PERSONAL_KEY_LEN.saturating_sub(padding)
}

/// What the signatures of an overview line say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// the spot (or command) is genuine: Spotweb would take it
    pub verified: bool,
    /// the spotter whose key signed it, when one did
    pub spotter_id: Option<String>,
    /// a key-2 command signed by a spotter's own key rather than Spotnet's moderator key: it may only remove a spot
    /// of that same spotter
    pub personal: bool,
    /// the spotter's key signed the message-id itself. A spotter id from the header signature alone can be
    /// replayed: that signature covers the title, header and nickname, not the message-id
    pub message_id_signed: bool,
    /// the header signature (over the title, header and nickname) holds: under Spotnet's key, or for a key-7 spot
    /// or a spotter's own command under the spotter's key
    pub title_signed: bool,
}

/// Checks the signatures of the spot (or moderator's command) whose overview gives `subject`, `from` and
/// `message_id` (with or without its brackets). `title` is the title `header::parse` read from `subject`.
///
/// The header signature is over text Atlas has already decoded (`from_utf8_lossy`, and on the overview
/// `undo_mojibake`), and RFC 2047 subjects aren't decoded first as Spotweb does: a non-ASCII title or nickname may
/// not rebuild the bytes that were signed. That decides the verdict only for keys 2 to 4 (a key-7 spot needs its
/// hashcash and message-id signature), and only the spotter id for a key-7 `From` without a user signature.
pub fn verify(title: &str, from: &str, message_id: &str) -> Verdict {
    let no = Verdict::default();
    let message_id = format!("<{}>", message_id.trim().trim_matches(['<', '>']));
    let Some(open) = from.find('<') else { return no };
    let address = from[open + 1..].strip_suffix('>').unwrap_or(&from[open + 1..]);
    let Some((local, header)) = address.split_once('@') else { return no };
    let fields: Vec<&str> = header.split('.').collect();
    let head = fields[0].as_bytes();
    if fields.len() < 6 || head.len() < 2 || !head[1].is_ascii_digit() {
        return no;
    }
    let key_id = i32::from(head[1] - b'0');
    let header_sig = fields[fields.len() - 1];
    if header_sig.is_empty() || title.is_empty() {
        return no;
    }
    // Spotweb drops the one byte before the `<` (the space) and nothing else, and wants a nickname left
    let poster = &from.as_bytes()[..open.saturating_sub(1)];
    if poster.is_empty() {
        return no;
    }
    let mut signed = Vec::from(title.as_bytes());
    signed.extend_from_slice(&header.as_bytes()[..header.len() - header_sig.len() - 1]);
    signed.extend_from_slice(poster);
    let Some(header_sig) = decode_base64(&unprepare_base64(header_sig)) else { return no };
    let (prepared_user_key, user_sig) = match local.split_once('.') {
        Some((k, s)) => (k, Some(unprepare_base64(s))),
        None => (local, None),
    };
    let user_key = unprepare_base64(prepared_user_key);
    let user_modulus = decode_base64(&user_key).unwrap_or_default();
    let by_user = || !user_modulus.is_empty() && rsa_sha1(&signed, &header_sig, &user_modulus);

    let mut verdict = Verdict::default();
    if key_id == SELF_SIGNED_KEY {
        // the hashcash is what Spotweb requires; the header signature, when it holds, names the spotter
        verdict.verified = hashcash(&message_id);
        if verdict.verified && by_user() {
            verdict.spotter_id = spotter_id(&user_key);
            verdict.title_signed = true;
        }
    } else if let Some((_, modulus)) = SPOTNET_KEYS.iter().find(|(k, _)| *k == key_id) {
        if key_id == 2 && personal_key_length(prepared_user_key.len(), user_key.len()) {
            verdict.personal = true;
            verdict.verified = hashcash(&message_id);
            if verdict.verified && by_user() {
                verdict.spotter_id = spotter_id(&user_key);
                verdict.title_signed = true;
            }
        } else {
            let modulus = decode_base64(modulus).expect("Spotnet's keys are base64");
            verdict.verified = rsa_sha1(&signed, &header_sig, &modulus);
            verdict.title_signed = verdict.verified;
        }
    }
    // a `From` with the spotter's key and signature: the message-id must be signed with that key too (a key that
    // won't decode signs nothing)
    if verdict.verified
        && let Some(user_sig) = user_sig.filter(|s| !s.is_empty())
        && !user_key.is_empty()
    {
        let sig = decode_base64(&user_sig).unwrap_or_default();
        verdict.verified = !user_modulus.is_empty() && rsa_sha1(message_id.as_bytes(), &sig, &user_modulus);
        if verdict.verified {
            verdict.spotter_id = spotter_id(&user_key);
            verdict.message_id_signed = true;
        }
    }
    if !verdict.verified {
        return no;
    }
    verdict
}

/// Whether the SHA-1 of `message_id` (with its brackets) starts with four zero hex digits.
fn hashcash(message_id: &str) -> bool {
    let digest = Sha1::digest(message_id.as_bytes());
    digest[0] == 0 && digest[1] == 0
}

/// The DER prefix of a PKCS #1 v1.5 SHA-1 `DigestInfo`.
const SHA1_DIGEST_INFO: [u8; 15] =
    [0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14];

/// Whether `sig` is an RSA PKCS #1 v1.5 SHA-1 signature of `message` under the key with `modulus` (big-endian)
/// and exponent 65537. Spotnet's own keys are 384 bits, below what crypto libraries take, hence this by hand.
fn rsa_sha1(message: &[u8], sig: &[u8], modulus: &[u8]) -> bool {
    let n = BigUint::from_bytes_be(modulus);
    let k = n.bits().div_ceil(8) as usize;
    // the 0x00 0x01, at least 8 0xff, the 0x00, the DigestInfo and the hash
    // and, as OpenSSL wants, a signature exactly as long as the key
    if k < 3 + 8 + SHA1_DIGEST_INFO.len() + 20 || sig.len() != k {
        return false;
    }
    let s = BigUint::from_bytes_be(sig);
    if s >= n {
        return false;
    }
    let m = s.modpow(&BigUint::from(65_537u32), &n).to_bytes_be();
    let mut expected = vec![0xff; k];
    expected[0] = 0;
    expected[1] = 1;
    let tail = k - SHA1_DIGEST_INFO.len() - 20;
    expected[tail - 1] = 0;
    expected[tail..tail + SHA1_DIGEST_INFO.len()].copy_from_slice(&SHA1_DIGEST_INFO);
    expected[k - 20..].copy_from_slice(&Sha1::digest(message));
    // `to_bytes_be` drops the leading zero bytes
    m.len() <= k && expected[..k - m.len()].iter().all(|b| *b == 0) && expected[k - m.len()..] == m[..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    // overview lines from free.pt, October 2026
    const SPOT_ID: &str = "<vKCGetIwSd0z2PcagDlUt@spot.net>";
    const SPOT_TITLE: &str = "Hitster.S01E01.DUTCH.1080p.WEB.h264-TRIPEL";
    const SPOT_FROM: &str = "Wasbeertje <r2ceRUXK82cxSlhCLnbGXUVIA6-srpbJJqYqtJfXLdubdAMMD6T0G-pcPdZe619uuF.Q4LGk2NHIuwiRJIi7V6sGnlCa0V0j4gScJAnpUHO8wU3fqtPFnYmKvzMNSIhlNPI@17a09b04d11b11c06c11z01.1651448731.20.1792828369.1.NL.Pi0aiE0W9LUv0UOMnVl8aFaUMKMM-sjxZ5m9yv2nrJeDHuWKO-pr-pvPfrvqR8tp9CY>";
    const MOD_ID: &str = "iWNrGQFeatMqnrcagoOav@spot.net";
    const MOD_TITLE: &str =
        "DISPOSE vhcUXQS5B5HaOrnqfQX9J1l5FoVEreXH@spot.net - DC200726TL&CM (Zie image previewlink ivm NTD)";
    const MOD_FROM: &str =
        "Afzender <10@12a01.1.10.1792834218.N5I.mod.vLCJUTdtVmLJtnj7fW5fpdlHfnAUuqeCGIzJr4AAVImZ9QICSsiJnehDrC7-pogB5>";
    const OWN_ID: &str = "x9FBYiqqYRYTGHcagIqb8.0.J6j5rpg31fwhGPcag.Ln42@spot.net";
    const OWN_TITLE: &str =
        "DISPOSE x9FBYiqqYRYTGHcagIqb8@spot.net - Rescued.2026.1080p.AMZN.WEB-DL.H.264-GP-M-NLsubs-Retail-Multi";
    const OWN_FROM: &str = "GPNZBPosterGroep <4CGr2alVHshWG91Ajiozvi3D5D1MPD8Iwh5veSiUvGdrhFl5LBcyN5jV6GwHXqZB.pXjyxG1fqwyJa5G-sjJlChIwBKawpo0ooSTKvOHKZuwslnqnM27W1xO9CnX2irBvb@12a01.999.10.1792828292.1.NL.izwJAvjPDylBpOFDpL9dN5cp-sxozQ73-sBW-sER-pKECT4rDFo-pNPWI2IIp2c3cOelu>";

    #[test]
    fn a_self_signed_spot() {
        let v = verify(SPOT_TITLE, SPOT_FROM, SPOT_ID);
        assert!(v.verified && !v.personal && v.message_id_signed);
        assert_eq!(v.spotter_id, spotter_id("r2ceRUXK82cxSlhCLnbGXUVIA6-srpbJJqYqtJfXLdubdAMMD6T0G-pcPdZe619uuF"));
        assert!(v.spotter_id.is_some());
        // the message-id is what the hashcash and the spotter's signature hold
        assert!(!verify(SPOT_TITLE, SPOT_FROM, MOD_ID).verified);
        // a header signature that doesn't hold costs the spotter id, as in Spotweb, not the spot
        let other = verify("Hitster.S01E02", SPOT_FROM, SPOT_ID);
        assert!(other.verified);
        // someone else's key with this spotter's message-id signature
        let forged = SPOT_FROM.replacen("r2ceRUXK", "r2ceRUXL", 1);
        assert!(!verify(SPOT_TITLE, &forged, SPOT_ID).verified);
    }

    #[test]
    fn a_moderators_command() {
        let v = verify(MOD_TITLE, MOD_FROM, MOD_ID);
        assert_eq!(v, Verdict { verified: true, title_signed: true, ..Verdict::default() });
        // the signed title names the spot: another spot's id breaks it
        let other = MOD_TITLE.replace("vhcUXQS5", "vhcUXQS6");
        assert_eq!(verify(&other, MOD_FROM, MOD_ID), Verdict::default());
        // and so does another nickname
        let renamed = MOD_FROM.replace("Afzender", "Afzendr");
        assert!(!verify(MOD_TITLE, &renamed, MOD_ID).verified);
        // a key Spotweb doesn't know
        let key5 = MOD_FROM.replace("@12a01", "@15a01");
        assert!(!verify(MOD_TITLE, &key5, MOD_ID).verified);
    }

    #[test]
    fn a_spotters_own_dispose() {
        let v = verify(OWN_TITLE, OWN_FROM, OWN_ID);
        assert!(v.verified && v.personal && v.message_id_signed && v.title_signed);
        // another target in the title: the spotter's header signature no longer holds
        let other = OWN_TITLE.replace("x9FBYiqq", "x9FBYiqr");
        let v = verify(&other, OWN_FROM, OWN_ID);
        assert!(v.verified && v.message_id_signed && !v.title_signed);
        assert_eq!(v.spotter_id, spotter_id("4CGr2alVHshWG91Ajiozvi3D5D1MPD8Iwh5veSiUvGdrhFl5LBcyN5jV6GwHXqZB"));
        assert!(v.spotter_id.is_some());
    }

    #[test]
    fn odd_nicknames() {
        // a multibyte character right before the `<`: the byte before it goes, as in Spotweb, and nothing panics
        let tight = SPOT_FROM.replacen("Wasbeertje <", "Né<", 1);
        assert!(verify(SPOT_TITLE, &tight, SPOT_ID).verified);
        // no nickname left once the byte before the `<` goes
        let bare = SPOT_FROM.replacen("Wasbeertje <", "x<", 1);
        assert_eq!(verify(SPOT_TITLE, &bare, SPOT_ID), Verdict::default());
        // a user key that won't decode can't have signed the message-id
        let garbage =
            SPOT_FROM.replacen("r2ceRUXK82cxSlhCLnbGXUVIA6-srpbJJqYqtJfXLdubdAMMD6T0G-pcPdZe619uuF", "!!!", 1);
        assert_eq!(verify(SPOT_TITLE, &garbage, SPOT_ID), Verdict::default());
    }

    #[test]
    fn unsigned_or_not_a_spot() {
        assert_eq!(verify("t", "x <k@17a09z00.100.1.2.3.4.>", SPOT_ID), Verdict::default());
        assert_eq!(verify("t", "someone <abc@example.com>", SPOT_ID), Verdict::default());
        assert_eq!(verify("", SPOT_FROM, SPOT_ID), Verdict::default());
    }

    fn boundary_from(key: &str, user_sig: Option<&str>) -> String {
        let user_sig = user_sig.map(|sig| format!(".{sig}")).unwrap_or_default();
        format!("Fixture <{key}{user_sig}@12a01.999.1.2.3.NL.eA>")
    }

    fn assert_boundary_verdict(key: &str, personal: bool) {
        let verdict = verify("Boundary.Header", &boundary_from(key, None), SPOT_ID);
        assert_eq!(verdict, Verdict { verified: personal, personal, ..Verdict::default() });
        // All synthetic moduli here are below the minimum RSA encoding size.
        // A hashcash-only classification must not invent signed identity.
        assert!(!verdict.title_signed && !verdict.message_id_signed && verdict.spotter_id.is_none());
        assert_eq!(verify("Boundary.Header", &boundary_from(key, Some("!!!")), SPOT_ID), Verdict::default());
    }

    #[test]
    fn personal_key_boundary_counts_reference_padding_for_every_short_modulus() {
        // Literal Base64 lengths, before/after omitted '=' restoration.
        for (bytes, unpadded_len, padded_len, personal) in [
            (32, 43, 44, false),
            (33, 44, 44, false),
            (34, 46, 48, false),
            (35, 47, 48, false),
            (36, 48, 48, false),
            (37, 50, 52, true),
            (38, 51, 52, true),
            (39, 52, 52, true),
            (40, 54, 56, true),
            (41, 55, 56, true),
            (42, 56, 56, true),
        ] {
            let padded = STANDARD.encode(vec![0u8; bytes]);
            let unpadded = padded.trim_end_matches('=');
            assert_eq!((unpadded.len(), padded.len()), (unpadded_len, padded_len));
            assert_eq!(decode_base64(unpadded).unwrap().len(), bytes);
            assert_boundary_verdict(unpadded, personal);
            assert_boundary_verdict(&padded, personal);
        }
    }

    #[test]
    fn personal_key_padding_precedes_restoring_the_prepared_alphabet() {
        // Spotweb pads the prepared input BEFORE '-s' shrinks to '/'. For two
        // escapes,52 prepared bytes restore to50 with no padding: not personal.
        for (escapes, prepared_personal) in [(1, true), (2, false), (3, true), (4, true)] {
            for (symbol, escaped) in [("/", "-s"), ("+", "-p")] {
                let bare = format!("{}{}", symbol.repeat(escapes), "A".repeat(50 - escapes));
                let prepared = bare.replace(symbol, escaped);
                assert_eq!(decode_base64(&bare).unwrap().len(), 37);
                assert_eq!((bare.len(), prepared.len()), (50, 50 + escapes));
                assert_boundary_verdict(&bare, true);
                assert_boundary_verdict(&prepared, prepared_personal);
                assert_boundary_verdict(&format!("{bare}=="), true);
                assert_boundary_verdict(&format!("{prepared}=="), true);
            }
        }
    }

    #[test]
    fn short_personal_keys_cannot_authorize_a_disposal_by_hashcash_alone() {
        for bytes in 37..=42 {
            let padded = STANDARD.encode(vec![0xFFu8; bytes]);
            let from = boundary_from(padded.trim_end_matches('='), None);
            let overview = crate::nntp::Overview {
                subject: "DISPOSE target@spot.net - Boundary.Header".into(),
                from,
                message_id: SPOT_ID.into(),
                ..Default::default()
            };
            let verdict = verify(&overview.subject, &overview.from, SPOT_ID);
            assert!(verdict.verified && verdict.personal);
            assert!(!verdict.title_signed && !verdict.message_id_signed && verdict.spotter_id.is_none());
            assert!(
                crate::spotnet::ingest::removal(&overview, 1).is_none(),
                "personal classification cannot grant moderator authority"
            );
        }
    }

    #[test]
    fn personal_key_length_comparison_does_not_add_past_usize() {
        assert!(personal_key_length(usize::MAX, usize::MAX));
        assert!(!personal_key_length(52, 50));
        assert!(personal_key_length(51, 50));
    }

    #[test]
    fn rsa_rejects_garbage() {
        let modulus = decode_base64(SPOTNET_KEYS[0].1).unwrap();
        assert!(!rsa_sha1(b"x", &[], &modulus));
        assert!(!rsa_sha1(b"x", &[0xff; 49], &modulus));
        assert!(!rsa_sha1(b"x", &[1; 48], &modulus));
        assert!(!rsa_sha1(b"x", &[1; 47], &modulus));
        assert!(!rsa_sha1(b"x", &[1; 8], &[0xff; 8]));
    }
}
