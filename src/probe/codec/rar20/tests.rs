use super::*;

#[test]
fn decodes_rar_2_lz() {
    for (name, data) in [
        ("audio", &include_bytes!("../../../../tests/fixtures/rar3/rar250_audio.rar")[..]),
        ("biglz", include_bytes!("../../../../tests/fixtures/rar3/rar250_biglz.rar")),
        ("solid", include_bytes!("../../../../tests/fixtures/rar3/rar250_solid.rar")),
        ("audio_text", include_bytes!("../../../../tests/fixtures/rar3/unpack20_audio_text.rar")),
        ("keep_tables", include_bytes!("../../../../tests/fixtures/rar3/unpack20_keep_tables.rar")),
        ("multiblock", include_bytes!("../../../../tests/fixtures/rar3/unpack20_multiblock.rar")),
    ] {
        assert!(super::super::rar3::tests::check(name, data, &[20, 26], rar20) > 0, "{name}: nothing checked");
    }
}
