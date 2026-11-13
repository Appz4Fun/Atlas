//! A file posted whole, read by byte range. Its articles hold equal pieces of it, all but the last, so where a byte
//! lies says which article holds it. zipview's `ZipIndex` asks for the ranges a ZIP needs, its directory and one
//! entry's bytes, and only the articles holding them are fetched: never the whole archive.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::BTreeMap;

use crate::postprocess::step::StepError;

/// Why a ranged read stopped.
#[derive(Debug)]
pub enum Halt {
    /// what zipview made of the bytes
    Zip(zipview::ZipReadError),
    /// an article not there, a budget spent, or a piece not the size the others say it is
    Unread,
    /// the step's own trouble: a server down, the deadline, a shutdown
    Step(StepError),
}

impl From<zipview::ZipReadError> for Halt {
    fn from(e: zipview::ZipReadError) -> Self {
        Halt::Zip(e)
    }
}

impl From<StepError> for Halt {
    fn from(e: StepError) -> Self {
        Halt::Step(e)
    }
}

/// Fetches a file's article by part number (1 based) and gives its decoded bytes. None: not there, or not to be read.
pub type Fetch<'a> = dyn FnMut(i64) -> Result<Option<Vec<u8>>, StepError> + 'a;

/// A file's pieces as its articles hold them, the ones read so far kept.
#[derive(Debug, Clone)]
pub struct Posted {
    len: u64,
    piece: u64,
    pieces: u64,
    held: BTreeMap<u64, Vec<u8>>,
}

impl Posted {
    /// A file of `pieces` articles from its first and last, decoded: every one but the last holds as many bytes as
    /// the first. None: a last bigger than the first, or an empty one, which no poster's even cut makes.
    pub fn new(pieces: u64, first: Vec<u8>, last: Vec<u8>) -> Option<Posted> {
        let mut held = BTreeMap::new();
        if pieces == 1 {
            let len = first.len() as u64;
            held.insert(1, first);
            return Some(Posted { len, piece: len.max(1), pieces, held });
        }
        let (piece, tail) = (first.len() as u64, last.len() as u64);
        if pieces < 2 || piece == 0 || tail == 0 || tail > piece {
            return None;
        }
        let len = piece.checked_mul(pieces - 1)?.checked_add(tail)?;
        held.insert(1, first);
        held.insert(pieces, last);
        Some(Posted { len, piece, pieces, held })
    }

    /// the whole file's length
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fills `buf` from the file at `offset`, fetching the articles it needs that aren't held yet.
    pub fn read(&mut self, offset: u64, buf: &mut [u8], fetch: &mut Fetch<'_>) -> Result<(), Halt> {
        let end = offset.checked_add(buf.len() as u64).filter(|e| *e <= self.len).ok_or(Halt::Unread)?;
        let mut at = offset;
        while at < end {
            let part = at / self.piece + 1;
            let start = (part - 1) * self.piece;
            if !self.held.contains_key(&part) {
                let number = i64::try_from(part).map_err(|_| Halt::Unread)?;
                let body = fetch(number)?.ok_or(Halt::Unread)?;
                let want = if part == self.pieces { self.len - start } else { self.piece };
                if body.len() as u64 != want {
                    return Err(Halt::Unread);
                }
                self.held.insert(part, body);
            }
            let body = self.held.get(&part).ok_or(Halt::Unread)?;
            let from = usize::try_from(at - start).map_err(|_| Halt::Unread)?;
            let have = body.get(from..).ok_or(Halt::Unread)?;
            let filled = usize::try_from(at - offset).map_err(|_| Halt::Unread)?;
            let room = buf.get_mut(filled..).ok_or(Halt::Unread)?;
            let n = have.len().min(room.len());
            room.get_mut(..n).ok_or(Halt::Unread)?.copy_from_slice(have.get(..n).ok_or(Halt::Unread)?);
            at += n as u64;
        }
        Ok(())
    }

    /// `len` bytes from `offset`, read as `read` does.
    pub fn bytes(&mut self, offset: u64, len: u64, fetch: &mut Fetch<'_>) -> Result<Vec<u8>, Halt> {
        let n = usize::try_from(len).map_err(|_| Halt::Unread)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(n).map_err(|_| Halt::Unread)?;
        buf.resize(n, 0);
        self.read(offset, &mut buf, fetch)?;
        Ok(buf)
    }

    /// The ZIP's directory, by zipview, and its end from the directory's first byte on: what `zip::list_tail` reads
    /// whole however long the directory runs. None: zipview couldn't open it, or an article it needed wasn't read.
    pub fn zip_directory(&mut self, fetch: &mut Fetch<'_>) -> Result<Option<(zipview::ZipIndex, Vec<u8>)>, StepError> {
        let mut opener = zipview::ZipIndexOpener::new(self.len);
        let mut from = self.len;
        loop {
            let range = match opener.next_read() {
                Ok(Some(range)) => range,
                Ok(None) => break,
                Err(_) => return Ok(None),
            };
            let bytes = match self.bytes(range.start, range.end - range.start, fetch) {
                Ok(bytes) => bytes,
                Err(Halt::Step(e)) => return Err(e),
                Err(_) => return Ok(None),
            };
            if opener.supply(range.start, &bytes).is_err() {
                return Ok(None);
            }
            from = from.min(range.start);
        }
        let Ok(index) = opener.finish() else { return Ok(None) };
        match self.bytes(from, self.len - from, fetch) {
            Ok(tail) => Ok(Some((index, tail))),
            Err(Halt::Step(e)) => Err(e),
            Err(_) => Ok(None),
        }
    }

    /// An entry's bytes, decompressed, by zipview: its local header, then its data. None: it didn't read.
    pub fn zip_entry(
        &mut self,
        index: &zipview::ZipIndex,
        entry: &zipview::ZipEntry,
        fetch: &mut Fetch<'_>,
    ) -> Result<Option<Vec<u8>>, StepError> {
        match index.fetch_entry(entry, None, |offset: u64, buf: &mut [u8]| self.read(offset, buf, fetch)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Halt::Step(e)) => Err(e),
            Err(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::testkit::zip_of;
    use crate::archive::zip;

    /// a file cut into the pieces its articles would hold, and a fetch counting what it's asked for
    fn cut(file: &[u8], piece: usize) -> Vec<Vec<u8>> {
        file.chunks(piece).map(<[u8]>::to_vec).collect()
    }

    fn posted(pieces: &[Vec<u8>]) -> Posted {
        Posted::new(pieces.len() as u64, pieces[0].clone(), pieces[pieces.len() - 1].clone()).unwrap()
    }

    fn fetcher<'a>(
        pieces: &'a [Vec<u8>],
        asked: &'a mut Vec<i64>,
    ) -> impl FnMut(i64) -> Result<Option<Vec<u8>>, StepError> + 'a {
        move |part| {
            asked.push(part);
            Ok(pieces.get(usize::try_from(part - 1).unwrap()).cloned())
        }
    }

    #[test]
    fn a_range_reads_the_articles_it_spans_once() {
        let file: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let pieces = cut(&file, 300);
        let mut p = posted(&pieces);
        assert_eq!(p.len(), 1000);
        let mut asked = Vec::new();
        let mut fetch = fetcher(&pieces, &mut asked);
        assert_eq!(p.bytes(250, 400, &mut fetch).unwrap(), &file[250..650]);
        assert_eq!(p.bytes(310, 20, &mut fetch).unwrap(), &file[310..330]);
        // the first and last came with it
        assert_eq!(p.bytes(950, 50, &mut fetch).unwrap(), &file[950..]);
        drop(fetch);
        assert_eq!(asked, [2, 3]);
    }

    #[test]
    fn a_range_past_the_end_or_a_piece_of_another_size_doesnt_read() {
        let file = vec![1u8; 1000];
        let pieces = cut(&file, 300);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        assert!(matches!(p.bytes(900, 101, &mut fetcher(&pieces, &mut asked)), Err(Halt::Unread)));
        let mut wrong = pieces.clone();
        wrong[1].pop();
        assert!(matches!(p.bytes(300, 10, &mut fetcher(&wrong, &mut asked)), Err(Halt::Unread)));
        // a last piece bigger than the first is no even cut
        assert!(Posted::new(2, vec![0; 10], vec![0; 11]).is_none());
        assert!(Posted::new(2, vec![0; 10], vec![]).is_none());
    }

    /// a ZIP of `n` files with long names: its directory is about 100 bytes a file
    fn many(n: usize) -> Vec<u8> {
        let names: Vec<String> = (0..n).map(|i| format!("Some.Show.S01E{i:03}.1080p.WEB-DL.mkv.part")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|s| (s.as_str(), &b"x"[..])).collect();
        zip_of(&files)
    }

    /// what zipview's first read takes from the end: the end record, the longest comment and the ZIP64 records
    const TAIL_READ: usize = 65_557 + 76;

    #[test]
    fn a_directory_longer_than_the_last_article_is_read_from_the_articles_it_spans() {
        let file = many(2000);
        let pieces = cut(&file, 64 << 10);
        // the last article alone holds part of it
        assert!(!zip::list_tail(&pieces[pieces.len() - 1]).complete);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        let (index, tail) = p.zip_directory(&mut fetcher(&pieces, &mut asked)).unwrap().unwrap();
        assert_eq!(index.len(), 2000);
        let listing = zip::list_tail(&tail);
        assert!(listing.complete);
        assert_eq!(listing.files.len(), 2000);
        // the articles from the directory's first to the last but one, and none holding only file data before it
        let from = (file.len() - tail.len()).min(file.len() - TAIL_READ) / (64 << 10) + 1;
        assert!(from > 2, "{from}");
        asked.sort_unstable();
        assert_eq!(asked, (from as i64..pieces.len() as i64).collect::<Vec<_>>());
    }

    #[test]
    fn an_entry_is_read_from_its_own_articles() {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        let file = zip_of(&[("big.bin", &big), ("Next.nzb", b"<nzb>hello</nzb>")]);
        let pieces = cut(&file, 64 << 10);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        let mut fetch = fetcher(&pieces, &mut asked);
        let (index, _) = p.zip_directory(&mut fetch).unwrap().unwrap();
        let entry = index.by_name("Next.nzb").unwrap().clone();
        assert_eq!(p.zip_entry(&index, &entry, &mut fetch).unwrap().unwrap(), b"<nzb>hello</nzb>");
        drop(fetch);
        // the end's read reaches into the last but one; nothing more of big.bin's is read
        assert_eq!(asked, [pieces.len() as i64 - 1]);
    }

    #[test]
    fn duplicate_names_keep_their_own_payloads_in_directory_order() {
        let file = zip_of(&[("Next.nzb", b"first"), ("Next.nzb", b"second")]);
        let pieces = cut(&file, 64);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        let mut fetch = fetcher(&pieces, &mut asked);
        let (index, _) = p.zip_directory(&mut fetch).unwrap().unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(p.zip_entry(&index, &index.entries()[0], &mut fetch).unwrap().unwrap(), b"first");
        assert_eq!(p.zip_entry(&index, &index.entries()[1], &mut fetch).unwrap().unwrap(), b"second");
    }

    #[test]
    fn an_entry_on_another_zip_disk_does_not_fetch_a_same_offset_article() {
        let before = vec![0; 130_000];
        let after = vec![0; 300_000];
        let mut file = zip_of(&[("before.bin", &before), ("Next.nzb", b"<nzb/>"), ("after.bin", &after)]);
        let at = file.windows(4).enumerate().filter_map(|(at, w)| (w == b"PK\x01\x02").then_some(at)).nth(1).unwrap();
        file[at + 34..at + 36].copy_from_slice(&1u16.to_le_bytes());
        let pieces = cut(&file, 64 << 10);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        let (index, _) = p.zip_directory(&mut fetcher(&pieces, &mut asked)).unwrap().unwrap();
        let reads_before = asked.clone();
        let entry = index.by_name("Next.nzb").unwrap();
        assert!(p.zip_entry(&index, entry, &mut fetcher(&pieces, &mut asked)).unwrap().is_none());
        assert_eq!(asked, reads_before, "a different disk's offset must not fetch a local article");
    }

    #[test]
    fn a_damaged_directory_reads_as_none() {
        let mut file = many(20);
        let at = file.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        file[at] = b'X';
        let pieces = cut(&file, 512);
        let mut p = posted(&pieces);
        let mut asked = Vec::new();
        assert!(p.zip_directory(&mut fetcher(&pieces, &mut asked)).unwrap().is_none());
    }

    #[test]
    fn a_step_error_comes_through() {
        let file = many(200);
        let pieces = cut(&file, 4096);
        let mut p = posted(&pieces);
        let mut stop = |_| Err(StepError::Stopped);
        assert!(matches!(p.zip_directory(&mut stop), Err(StepError::Stopped)));
    }
}
