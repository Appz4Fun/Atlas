//! Source-first compatibility/admission controls. No database is needed here.
use super::{SidecarRecord, StepMutation, StepWrite};
use crate::postprocess::result_value::{Value, encode_write_v1};
use sha2::{Digest, Sha256};

fn encode(step: i64, tag: &str, values: &[Value], used: &mut usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_write_v1(step, tag, values, used, &mut |part| bytes.extend_from_slice(part)).unwrap();
    bytes
}

// Catches tag/type/length/order/endian changes, including treating NUL as a terminator.
#[test]
fn v1_value_wire_golden_preserves_null_signed_float_text_and_blob_bits() {
    let mut used = 0;
    let bytes = encode(-1, "t", &[
        Value::Null, Value::Integer(i64::MIN), Value::Integer(i64::MAX),
        Value::Real(-0.0), Value::Text("é\0".into()), Value::Blob(vec![0, 255]),
    ], &mut used);
    let expected: &[u8] = &[
        255,255,255,255,255,255,255,255, // step -1
        1,0,0,0,0,0,0,0,116, // tag "t"
        0, // NULL
        1,0,0,0,0,0,0,0,128, // i64::MIN
        1,255,255,255,255,255,255,255,127, // i64::MAX
        2,0,0,0,0,0,0,0,128, // negative zero, exact IEEE bits
        3,3,0,0,0,0,0,0,0,195,169,0, // UTF-8 bytes, not scalar count
        4,2,0,0,0,0,0,0,0,0,255, // binary, not text
        255, // write terminator
    ];
    assert_eq!(bytes, expected);
    assert_eq!(used, 6, "v1 charges tag/text/blob payloads, not framing or numeric fields");
    assert_ne!(encode(0, "", &[Value::Null], &mut 0), encode(0, "", &[Value::Text(String::new())], &mut 0));
    assert_ne!(encode(0, "", &[Value::Text(String::new())], &mut 0), encode(0, "", &[Value::Blob(vec![])], &mut 0));
}

// Catches resetting the shared limit per write and wrapping the byte counter.
#[test]
fn v1_write_budget_is_aggregate_and_checked_at_the_old_exact_boundary() {
    let mut used = 64 * 1024 * 1024 - 3;
    encode(1, "x", &[Value::Text("yz".into())], &mut used);
    assert_eq!(used, 64 * 1024 * 1024);
    assert!(encode_write_v1(2, "", &[Value::Blob(vec![1])], &mut used, &mut |_| {}).is_err());
    let mut overflow = usize::MAX;
    assert!(encode_write_v1(1, "x", &[], &mut overflow, &mut |_| {}).is_err());
}

fn category() -> StepWrite {
    StepWrite::for_mutation(7, StepMutation::SetCategory { category: 2045 })
}

// Catches accepting a forged shadow, caller SQL or a deferred callback as native authority.
#[test]
fn native_envelope_refuses_sql_parameter_and_callback_mismatches() {
    assert!(StepWrite::new("select 1", vec![]).native_mutation().is_err());
    let mut sql = category(); sql.sql = "select 1";
    let mut value = category(); value.params[1] = Value::Integer(5040);
    let late = category().with_late_params(Box::new(|| panic!("native validation executed a legacy callback")));
    for write in [sql, value, late] {
        assert!(write.native_mutation_for(7).is_err());
        assert!(write.encode_native_v1(4, &mut 0, &mut |_| {}).is_err());
    }
    assert!(category().native_mutation_for(8).is_err());
    assert!(category().native_mutation_for(7).is_ok());
}

// Catches loss of native participant resolution when moving the representation.
#[test]
fn native_envelope_does_not_turn_numeric_sidecar_targets_into_authority() {
    let write = StepWrite::for_mutation(7, StepMutation::KeepNfoForLegacyTarget {
        target_id: 8, file: "a.nfo".into(), text: "retained".into(),
    });
    assert!(write.native_mutation_for(7).is_err());
    assert!(write.native_mutation_for(8).is_err());
}

// Catches sidecar field reordering, dropping NULL versus empty, or unsigned timestamp conversion.
#[test]
fn native_sidecar_envelope_retains_exact_nullable_order_and_detects_changes() {
    let mut write = StepWrite::for_mutation(7, StepMutation::KeepSidecar(SidecarRecord {
        name: "n\0".into(), group: "g".into(), file: None, kind: "text".into(),
        release_name: Some(String::new()), segment_uid: None, text: Some("é".into()), at: i64::MIN,
    }));
    assert_eq!(write.params, vec![Value::Integer(7), Value::Text("n\0".into()), Value::Text("g".into()),
        Value::Null, Value::Text("text".into()), Value::Text(String::new()), Value::Null,
        Value::Text("é".into()), Value::Integer(i64::MIN)]);
    assert!(write.native_mutation_for(7).is_ok());
    write.params[5] = Value::Null;
    assert!(write.native_mutation_for(7).is_err());
}

// Catches changing the existing full write digest; literal bytes are independent of v1_fields.
#[test]
fn native_category_write_matches_fixed_v1_bytes() {
    let write = category();
    let mut actual = Vec::new();
    write.encode_native_v1(4, &mut 0, &mut |bytes| actual.extend_from_slice(bytes)).unwrap();
    let tag = b"insert or replace into release_category(release_id,category) values(?1,?2)";
    let mut expected = vec![4,0,0,0,0,0,0,0,74,0,0,0,0,0,0,0];
    expected.extend_from_slice(tag);
    expected.extend_from_slice(&[1,7,0,0,0,0,0,0,0,1,253,7,0,0,0,0,0,0,255]);
    assert_eq!(actual, expected);
    assert_eq!(format!("{:x}", Sha256::digest(actual)), "27a71347c692069acb30e0438b1deffbd77a4f6e3e2ee1572276804417b0fed9");
}
