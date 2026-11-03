use super::*;

#[test]
fn delta_adds_back_the_byte_dist_before() {
    let mut buf = [1, 2, 1, 1, 1, 1];
    assert_eq!(Filter::Delta { dist: 2 }.undo(&mut buf), 6);
    assert_eq!(buf, [1, 2, 2, 3, 3, 4]);
}

#[test]
fn properties_are_read_or_refused() {
    assert_eq!(Filter::from_xz(0x03, &[3]), Some(Filter::Delta { dist: 4 }));
    assert_eq!(Filter::from_xz(0x04, &[]), Some(Filter::Bcj { arch: Arch::X86, start: 0 }));
    assert_eq!(Filter::from_xz(0x07, &[0, 0x10, 0, 0]), Some(Filter::Bcj { arch: Arch::Arm, start: 0x1000 }));
    assert_eq!(Filter::from_xz(0x04, &[1]), None);
    assert_eq!(Filter::from_xz(0x0b, &[]), None);
}

#[test]
fn junk_and_short_buffers_never_panic() {
    let mut seed = 11u32;
    for n in 0..300 {
        let mut data: Vec<u8> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            })
            .collect();
        for arch in [Arch::X86, Arch::PowerPc, Arch::Ia64, Arch::Arm, Arch::ArmThumb, Arch::Sparc, Arch::Arm64] {
            let done = Filter::Bcj { arch, start: seed }.undo(&mut data);
            assert!(done <= data.len());
        }
    }
}
