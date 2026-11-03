//! Hand-built archives for tests; no archive tool is needed. bsdtar and zipfile read what these
//! build, apart from the deliberately broken cases in the tests.
use super::crc32 as crc;
fn le16(n: usize) -> [u8; 2] {
    (n as u16).to_le_bytes()
}
fn le32(n: usize) -> [u8; 4] {
    (n as u32).to_le_bytes()
}

// RAR4 block: crc16, type, flags, size, [add], fields
pub const R4_MARK: &[u8] = b"Rar!\x1a\x07\x00";
pub fn r4_block(ty: u8, flags: u16, fields: &[u8], add: Option<u32>) -> Vec<u8> {
    let flags = if add.is_some() { flags | 0x8000 } else { flags };
    let add = add.map_or(vec![], |a| a.to_le_bytes().to_vec());
    let h = [&[ty][..], &flags.to_le_bytes(), &le16(7 + add.len() + fields.len()), &add, fields].concat();
    [&(crc(&h) as u16).to_le_bytes()[..], &h].concat()
}
pub fn r4_main(flags: u16) -> Vec<u8> {
    r4_block(0x73, flags, &[0; 6], None)
}
pub fn r4_end() -> Vec<u8> {
    r4_block(0x7b, 0x4000, &[], None)
}
/// UNP_SIZE, HOST_OS 3, FILE_CRC 0, FTIME, UNP_VER 29, METHOD stored, NAME_SIZE, ATTR, name
pub fn r4_file_fields(name: &[u8], unp: u32) -> Vec<u8> {
    [
        &unp.to_le_bytes()[..],
        &[3, 0, 0, 0, 0],
        &0x5a21_5a21u32.to_le_bytes(),
        &[29, 0x30],
        &le16(name.len()),
        &0x20u32.to_le_bytes(),
        name,
    ]
    .concat()
}
pub fn r4_file(flags: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
    [r4_block(0x74, flags, &r4_file_fields(name, data.len() as u32), Some(data.len() as u32)), data.to_vec()].concat()
}
pub fn rar4(blocks: &[Vec<u8>]) -> Vec<u8> {
    [R4_MARK, &blocks.concat()].concat()
}

// RAR5 block: crc32, size, type, flags, [extra size], [data size], fields, extra; then data
pub const R5_MARK: &[u8] = b"Rar!\x1a\x07\x01\x00";
pub fn vint(mut n: u64) -> Vec<u8> {
    let mut o = Vec::new();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            o.push(b);
            return o;
        }
        o.push(b | 0x80);
    }
}
pub fn r5_block(ty: u64, fields: &[u8], extra: &[u8], data: Option<&[u8]>) -> Vec<u8> {
    let flags = u64::from(!extra.is_empty()) | if data.is_some() { 2 } else { 0 };
    let extra_size = if extra.is_empty() { vec![] } else { vint(extra.len() as u64) };
    let data_size = data.map_or(vec![], |d| vint(d.len() as u64));
    let inner = [&vint(ty)[..], &vint(flags), &extra_size, &data_size, fields, extra].concat();
    let body = [vint(inner.len() as u64), inner].concat();
    [&crc(&body).to_le_bytes()[..], &body, data.unwrap_or(&[])].concat()
}
pub fn r5_main() -> Vec<u8> {
    r5_block(1, &vint(0), &[], None)
}
pub fn r5_end() -> Vec<u8> {
    r5_block(5, &vint(0), &[], None)
}
/// file flags, unpacked size, attributes 0x20, [mtime if 2], [crc 0 if 4], compression 0, host 0, name length, name
pub fn r5_file_fields(file_flags: u64, name: &[u8], unp: u64) -> Vec<u8> {
    let mtime: &[u8] = if file_flags & 2 != 0 { &[0, 0, 0, 0x60] } else { &[] };
    let data_crc: &[u8] = if file_flags & 4 != 0 { &[0; 4] } else { &[] };
    [&vint(file_flags)[..], &vint(unp), &vint(0x20), mtime, data_crc, &[0, 0], &vint(name.len() as u64), name].concat()
}
/// `ty` 2 file, 3 service. `file_flags` 4 = crc present, 5 = a directory too. A data area is always flagged, as WinRAR does
pub fn r5_file(ty: u64, file_flags: u64, name: &[u8], extra: &[u8], data: &[u8]) -> Vec<u8> {
    r5_block(ty, &r5_file_fields(file_flags, name, data.len() as u64), extra, Some(data))
}
/// file extra record 1: size, type 1, version 0, flags 0, kdf 15, salt 16, iv 16
pub fn r5_crypt_record() -> Vec<u8> {
    let b = [&[1u8, 0, 0, 15][..], &[0x53; 16], &[0x49; 16]].concat();
    [vint(b.len() as u64), b].concat()
}
/// encryption header, type 4: version 0, flags 1, kdf 15, salt 16, check 12
pub fn r5_crypt_header() -> Vec<u8> {
    r5_block(4, &[&[0u8, 1, 15][..], &[0x53; 16], &[0x43; 12]].concat(), &[], None)
}
pub fn rar5(blocks: &[Vec<u8>]) -> Vec<u8> {
    [R5_MARK, &blocks.concat()].concat()
}

// 7z: magic, version, start crc, next offset, next size, next crc, packed bytes, next header
pub fn sz_num(n: u64) -> Vec<u8> {
    match n {
        0..=0x7f => vec![n as u8],
        0x80..=0x3fff => vec![0x80 | (n >> 8) as u8, n as u8],
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}
pub fn sz_wrap(packed: &[u8], next: &[u8]) -> Vec<u8> {
    let start =
        [&(packed.len() as u64).to_le_bytes()[..], &(next.len() as u64).to_le_bytes(), &crc(next).to_le_bytes()]
            .concat();
    [&b"7z\xbc\xaf\x27\x1c\x00\x04"[..], &crc(&start).to_le_bytes(), &start, packed, next].concat()
}
pub const SZ_COPY: &[u8] = &[0x01, 0x01, 0x00];
pub const SZ_AES_DATA: &[u8] = &[0x01, 0x24, 0x06, 0xf1, 0x07, 0x01, 0x02, 0xd3, 0x00];
/// 04 streams for one folder with `coder`, split into `sizes` streams
pub fn sz_main_streams(pack_len: u64, sizes: &[u64], coder: &[u8]) -> Vec<u8> {
    let n = sz_num(pack_len);
    let sub = if sizes.len() == 1 {
        vec![]
    } else {
        [
            vec![0x0d],
            sz_num(sizes.len() as u64),
            vec![0x09],
            sizes[..sizes.len() - 1].iter().flat_map(|s| sz_num(*s)).collect(),
        ]
        .concat()
    };
    [
        &[0x04, 0x06, 0x00, 0x01, 0x09][..],
        &n,
        &[0x00, 0x07, 0x0b, 0x01, 0x00],
        coder,
        &[0x0c],
        &n,
        &[0x00, 0x08],
        &sub,
        &[0x00, 0x00],
    ]
    .concat()
}
/// 05 files. `empty_bits` is one byte of the empty stream vector
pub fn sz_files(names: &[&str], empty_bits: Option<u8>) -> Vec<u8> {
    let utf16: Vec<u8> = names.iter().flat_map(|n| n.encode_utf16().chain([0])).flat_map(u16::to_le_bytes).collect();
    let empty = empty_bits.map_or(vec![], |b| vec![0x0e, 0x01, b]);
    [
        &[0x05][..],
        &sz_num(names.len() as u64),
        &empty,
        &[0x11],
        &sz_num(utf16.len() as u64 + 1),
        &[0x00],
        &utf16,
        &[0x00],
    ]
    .concat()
}
pub fn sz_plain_header(pack_len: u64, sizes: &[u64], names: &[&str], empty_bits: Option<u8>) -> Vec<u8> {
    [&[0x01][..], &sz_main_streams(pack_len, sizes, SZ_COPY), &sz_files(names, empty_bits), &[0x00]].concat()
}
/// raw LZMA1 and its 5 property bytes, from the crate's encoder (a .lzma file is 13 header bytes, then raw)
pub fn lzma_raw(plain: &[u8]) -> ([u8; 5], Vec<u8>) {
    let mut alone = Vec::new();
    lzma_rs::lzma_compress(&mut &plain[..], &mut alone).unwrap();
    (alone[..5].try_into().unwrap(), alone[13..].to_vec())
}
/// 17 EncodedHeader: a packed header at `pack_pos`, coder LZMA
pub fn sz_encoded_header(pack_pos: u64, pack_len: u64, unp: u64, props: &[u8; 5], crc32: u32) -> Vec<u8> {
    [
        &[0x17, 0x06][..],
        &sz_num(pack_pos),
        &[0x01, 0x09],
        &sz_num(pack_len),
        &[0x00, 0x07, 0x0b, 0x01, 0x00, 0x01, 0x23, 0x03, 0x01, 0x01, 0x05],
        props,
        &[0x0c],
        &sz_num(unp),
        &[0x0a, 0x01],
        &crc32.to_le_bytes(),
        &[0x00, 0x00],
    ]
    .concat()
}
/// `payload`, then an LZMA packed plain header listing `names`, laid out as 7-Zip does. One stream per name: 1 byte each, the last takes the rest
pub fn sz_encoded(payload: &[u8], names: &[&str]) -> Vec<u8> {
    let mut sizes = vec![1u64; names.len() - 1];
    sizes.push(payload.len() as u64 - sizes.len() as u64);
    let plain = sz_plain_header(payload.len() as u64, &sizes, names, None);
    let (props, packed) = lzma_raw(&plain);
    let next = sz_encoded_header(payload.len() as u64, packed.len() as u64, plain.len() as u64, &props, crc(&plain));
    sz_wrap(&[payload, &packed].concat(), &next)
}
/// EncodedHeader with folder LZMA then AES and bind pair (0, 1), as `7z a -p -mhe=on` writes it. bsdtar says "header is encrypted"
pub fn sz_encrypted_header(pack_pos: u64, pack_len: u64) -> Vec<u8> {
    [
        &[0x17, 0x06][..],
        &sz_num(pack_pos),
        &[0x01, 0x09],
        &sz_num(pack_len),
        &[
            0x00, 0x07, 0x0b, 0x01, 0x00, 0x02, 0x23, 0x03, 0x01, 0x01, 0x05, 0x5d, 0, 0, 0x10, 0, 0x24, 0x06, 0xf1,
            0x07, 0x01, 0x02, 0xd3, 0x00, 0x00, 0x01, 0x0c,
        ],
        &sz_num(100),
        &sz_num(pack_len),
        &[0x00, 0x00],
    ]
    .concat()
}

// ZIP: stored entries, sizes equal
pub fn zip_local(flags: u16, method: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
    [
        &b"PK\x03\x04\x14\x00"[..],
        &flags.to_le_bytes(),
        &method.to_le_bytes(),
        &[0; 4],
        &crc(data).to_le_bytes(),
        &le32(data.len()),
        &le32(data.len()),
        &le16(name.len()),
        &[0; 2],
        name,
        data,
    ]
    .concat()
}
pub fn zip_central(flags: u16, method: u16, name: &[u8], data: &[u8], offset: u32) -> Vec<u8> {
    [
        &b"PK\x01\x02\x14\x00\x14\x00"[..],
        &flags.to_le_bytes(),
        &method.to_le_bytes(),
        &[0; 4],
        &crc(data).to_le_bytes(),
        &le32(data.len()),
        &le32(data.len()),
        &le16(name.len()),
        &[0; 12],
        &offset.to_le_bytes(),
        name,
    ]
    .concat()
}
pub fn zip_eocd(entries: u16, cd_size: u32, cd_offset: u32, comment: &[u8]) -> Vec<u8> {
    [
        &b"PK\x05\x06\0\0\0\0"[..],
        &entries.to_le_bytes(),
        &entries.to_le_bytes(),
        &cd_size.to_le_bytes(),
        &cd_offset.to_le_bytes(),
        &le16(comment.len()),
        comment,
    ]
    .concat()
}
pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut body, mut cd) = (Vec::new(), Vec::new());
    for (n, d) in files {
        cd.extend(zip_central(0, 0, n.as_bytes(), d, body.len() as u32));
        body.extend(zip_local(0, 0, n.as_bytes(), d));
    }
    let eocd = zip_eocd(files.len() as u16, cd.len() as u32, body.len() as u32, b"");
    [body, cd, eocd].concat()
}
