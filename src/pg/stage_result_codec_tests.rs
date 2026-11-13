//! Pure full-Visit goldens; real owner/lease/participant controls remain in the PG tests.
use super::{Applied, Claim, Lane, Visit};
use crate::postprocess::step::{NameChange, StepMutation, StepWrite};

fn claim() -> Claim {
    Claim {epoch:[1;32],catalog:[2;32],id:7,generation:0,serial:0,token:0,rerun:0,due:0,
        until:1,revision:0,lane:Lane::Db,operation:[3;32],request:[4;32],walk:None}
}
fn applied() -> Applied {
    Applied {release_id:7,claimed_gen:-2,claimed_due:Some(9),set_done:4,set_failed:8,clear:16,
        rename_clears:32,net_mask:64,db_mask:128,net_steps:256,db_attempts:Some(1),net_attempts:None,
        net_transient:Some(-1),net_next_at:Some(99),parts_seen:Some(0),queue_at:Some(123),
        error:Some("é\0".into()),writes:vec![
            (4,StepWrite::for_mutation(7,StepMutation::SetCategory {category:2045})),
            (8,StepWrite::for_mutation(7,StepMutation::ClearCategory)),
        ],names:vec![(16,NameChange {source:2,display_name:Some(String::new()),obfuscated:Some(false)})],
        ..Default::default()}
}

// Catches accidental changes to any existing framing/order or the surrounding Applied encoding.
#[test]
fn full_visit_preserves_literal_v1_result_digest() {
    let visit=Visit::prepare(&claim(),applied(),[5;32]).unwrap();
    let actual=visit.result.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    assert_eq!(actual,"c7d9591f65c8fc3ef8f030c36ba869f63b24abe47a4f611847a895091825ac0f");
    let mut reordered=applied();reordered.writes.reverse();
    assert_ne!(visit.result,Visit::prepare(&claim(),reordered,[5;32]).unwrap().result);
    let mut changed=applied();changed.parts_seen=None;
    assert_ne!(visit.result,Visit::prepare(&claim(),changed,[5;32]).unwrap().result);
}

// Closed payload changes are bound to the digest; a result cannot grant another target.
// SQL shadows and deferred legacy callbacks no longer exist in StepWrite's public API.
#[test]
fn full_visit_refuses_wrong_target_and_binds_closed_mutation_payload() {
    let original=Visit::prepare(&claim(),applied(),[5;32]).unwrap();
    let mut changed=applied();
    changed.writes[0].1=StepWrite::for_mutation(7,StepMutation::SetCategory {category:2046});
    assert_ne!(original.result,Visit::prepare(&claim(),changed,[5;32]).unwrap().result);
    let mut wrong=applied();wrong.writes[0].1=StepWrite::for_mutation(8,StepMutation::ClearCategory);
    assert!(Visit::prepare(&claim(),wrong,[5;32]).is_err());
}
