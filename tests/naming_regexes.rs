//! The converted naming regexes.

use atlas::collections::subject_base;
use atlas::naming::regexes::*;

const SAMPLES: &str = include_str!("fixtures/naming_samples.tsv");

#[test]
fn every_rule_compiles() {
    let ids = every_id();
    assert_eq!(ids.len(), 1154);
    for id in ids {
        assert!(rule_by_id(id).is_some(), "{id}");
    }
    assert_eq!(group_regexes_built(), 118);
}

#[test]
fn the_seeders_own_samples_agree() {
    let mut checked = 0;
    for line in SAMPLES.lines().filter(|l| !l.starts_with('#')) {
        let mut fields = line.split('\t');
        let (id, subject, expected) = (fields.next().unwrap(), fields.next().unwrap(), fields.next().unwrap());
        let rule = rule_by_id(id.parse().unwrap()).unwrap();
        assert_eq!(rule.name_from(subject).as_deref(), Some(expected), "rule {id} on {subject}");
        checked += 1;
    }
    assert_eq!(checked, 1045);
}

#[test]
fn possessive_became_greedy() {
    let subject = r#"[AFL 2017 Round 8 NMFC vs SYD 720p] - "AFL 2017 Round 8 NMFC vs SYD 720p par2" yEnc"#;
    let rule = rule_by_id(1157).unwrap();
    assert_eq!(rule.name_from(subject).as_deref(), Some("AFL 2017 Round 8 NMFC vs SYD 720p"));
    assert!(names("alt.binaries.multimedia.sports", subject).iter().any(|n| n == "AFL 2017 Round 8 NMFC vs SYD 720p"));
    assert!(SAMPLES.lines().any(|l| l.starts_with("1157\t")));
}

#[test]
fn rules_come_in_file_order() {
    let cell = rules_for("alt.binaries.teevee");
    let ids: Vec<u32> = cell.get().unwrap().iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 89);
    let order = every_id();
    let at = |id: &u32| order.iter().position(|o| o == id).unwrap();
    assert!(ids.windows(2).all(|w| at(&w[0]) < at(&w[1])), "{ids:?}");
}

#[test]
fn names_run_on_part_2s_subject_base() {
    let subject = r#"Brought to you by Uploader [01/35] - "x.rar" yEnc (1/50)"#;
    let got = names("alt.binaries.boneless", &subject_base(subject));
    assert!(got.iter().any(|n| n == "Brought to you by Uploader"), "{got:?}");
}

#[test]
fn a_long_subject_is_skipped() {
    assert!(names("alt.binaries.teevee", &"x".repeat(2_000)).is_empty());
}

#[test]
fn group_has_rules_table() {
    assert!(group_has_rules("alt.binaries.teevee"));
    assert!(group_has_rules("alt.binaries.test"));
    assert!(group_has_rules("ALT.BINARIES.TEEVEE"));
    assert!(!group_has_rules("alt.binaries.no.rules"));
}
