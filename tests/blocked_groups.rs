//! Portable public classification contract. Runtime/read-boundary oracles live
//! in indexer::native_blocked_contract, using the genuine native owner.
use atlas::config::Config;
const BLOCKED: &str = "alt.binaries.pictures.erotica.teen";
const GROUP: &str = "alt.binaries.test";
#[test]
fn config_filters_blocked_groups_without_falling_back_to_the_single_group() {
    let config = Config {
        group: BLOCKED.into(),
        groups: vec![BLOCKED.into(), GROUP.into(), "alt.binaries.yong.eroitca".into()],
        ..Config::default()
    };
    assert_eq!(config.tracked_groups(), vec![GROUP.to_string()]);
    assert_eq!(config.blocked_groups(), vec![BLOCKED.to_string(), "alt.binaries.yong.eroitca".to_string()]);
    let only = Config { group: BLOCKED.into(), groups: vec![BLOCKED.into()], ..Config::default() };
    assert_eq!(only.tracked_groups(), Vec::<String>::new());
}
