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

#[test]
fn table_reads_are_counted() {
    let data = [0u8; 64];
    let mut st = State {
        bits: Bits { data: &data, addr: 0, bit: 0 },
        safe: data.len(),
        old_table: [0; MC * 4],
        reps: [0; 4],
        rep_at: 0,
        last_dist: 0,
        last_len: 0,
        audio: [Audio::default(); 4],
        channel: 0,
        channel_delta: 0,
        tables_left: 0,
    };
    assert!(read_tables(&mut st).is_none());
    assert_eq!(st.bits.addr, 0, "nothing read once the allowance is spent");
}
