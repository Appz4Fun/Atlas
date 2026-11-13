//! Offline preservation of the four valuable asset directories. This module
//! hashes/compares files; it never copies, deletes or fetches an asset, opens a
//! database, or enables a reader. Invoke only under the operator's stopped-
//! writer/resource admission. An unfinished manifest is never verification.

use std::collections::BTreeSet;
use std::fs::Metadata;
use std::io::{BufRead,BufReader,BufWriter,Read,Write};
use std::path::{Component,Path};
use std::time::{Duration,Instant};

use anyhow::{Context,Result,bail,ensure};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};

#[derive(Clone,Copy,Debug,PartialEq,Eq,PartialOrd,Ord,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub(crate) enum Kind { Posters,Covers,Stills,Images }
pub(crate) const KINDS:[Kind;4]=[Kind::Posters,Kind::Covers,Kind::Stills,Kind::Images];
impl Kind {
    pub(crate) fn directory(self)->&'static str {match self {Self::Posters=>"posters",Self::Covers=>"covers",Self::Stills=>"stills",Self::Images=>"images"}}
}

/// All are hard bounds. The caller supplies its qualified source forecast;
/// this module never raises a bound or retries after a failure.
#[derive(Clone,Copy)]
pub(crate) struct Limits {
    pub max_files:u64,pub min_files:u64,pub max_file_bytes:u64,pub max_total_bytes:u64,
    pub read_bytes_per_second:u64,
}
impl Limits {
    fn check(self)->Result<()> {
        ensure!(self.max_files>0 && self.max_files<=100_000 && self.min_files<=self.max_files,"asset file-count admission");
        ensure!(self.max_file_bytes>0 && self.max_file_bytes<=256*1024*1024,"asset per-file admission");
        ensure!(self.max_total_bytes>0 && self.max_total_bytes<=64*1024*1024*1024,"asset total-byte admission");
        ensure!((1024*1024..=32*1024*1024).contains(&self.read_bytes_per_second),"asset IO rate admission");Ok(())
    }
}

struct Pacer {next:Instant,rate:u64}
impl Pacer {
    fn new(limits:Limits)->Self {Self {next:Instant::now(),rate:limits.read_bytes_per_second}}
    fn charge(&mut self,bytes:u64)->Result<()> {
        if let Some(wait)=self.next.checked_duration_since(Instant::now()) {std::thread::sleep(wait)}
        // No accumulated idle credit: a slow writer cannot buy a later burst.
        self.next=Instant::now()+Duration::from_secs_f64(bytes as f64/self.rate as f64);
        Ok(())
    }
}

struct Observed {kind:Kind,name:String,identity:Metadata}

fn recheck(home:&Path,observed:&[Observed],pacer:&mut Pacer)->Result<()> {
    for file in observed {pacer.charge(4096)?;unchanged(&home.join(file.kind.directory()).join(&file.name),&file.identity)?;}
    Ok(())
}

#[derive(Debug,PartialEq,Eq)]
pub(crate) struct Summary {pub files:u64,pub bytes:u64,pub manifest_sha256:[u8;32]}

#[derive(Serialize,Deserialize)]
#[serde(tag="type",rename_all="snake_case",deny_unknown_fields)]
pub(crate) enum Record {
    Header {version:u8},
    File {kind:Kind,name:String,bytes:u64,sha256:String},
    Directory {kind:Kind,absent:bool,files:u64,bytes:u64},
    Complete {files:u64,bytes:u64,sha256:String},
}

pub(crate) fn filename(name:&str)->Result<()> {
    ensure!(!name.is_empty() && name.len()<=255 && !name.contains('\\') && !name.contains('\0'),"invalid asset filename");
    let mut components=Path::new(name).components();
    ensure!(matches!(components.next(),Some(Component::Normal(_))) && components.next().is_none(),"asset filename must be one component");Ok(())
}

#[cfg(unix)]
fn same(a:&Metadata,b:&Metadata)->bool {
    use std::os::unix::fs::MetadataExt;
    a.dev()==b.dev() && a.ino()==b.ino() && a.mode()==b.mode() && a.len()==b.len()
      && a.mtime()==b.mtime() && a.mtime_nsec()==b.mtime_nsec()
      && a.ctime()==b.ctime() && a.ctime_nsec()==b.ctime_nsec()
}
#[cfg(not(unix))]
fn same(_a:&Metadata,_b:&Metadata)->bool {false}

fn directory(path:&Path)->Result<Option<Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)=>{ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(),"asset directory is not a physical directory");Ok(Some(metadata))},
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),
        Err(error)=>Err(error.into()),
    }
}

fn unchanged(path:&Path,before:&Metadata)->Result<()> {
    let after=std::fs::symlink_metadata(path)?;
    ensure!(same(before,&after),"asset source changed during verification");Ok(())
}

fn open_readonly(path:&Path)->Result<std::fs::File> {
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        Ok(std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC|libc::O_NONBLOCK).open(path)?)
    }
    #[cfg(not(unix))] {let _=path;bail!("asset identity verification requires Unix file identities")}
}

pub(crate) fn capture_file(home:&Path,path:&Path,limits:Limits)->Result<Summary> {
    limits.check()?;directory(home)?.context("asset home is missing")?;
    let home_canonical=home.canonicalize()?;
    let parent=path.parent().context("asset manifest parent required")?.canonicalize()?;
    for kind in KINDS {ensure!(!parent.starts_with(home_canonical.join(kind.directory())),"manifest output must be outside asset directories");}
    let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let file=options.open(path)?;let mut output=BufWriter::with_capacity(64*1024,file);
    let summary=capture(home,&mut output,limits)?;output.flush()?;output.get_ref().sync_all()?;
    std::fs::File::open(parent)?.sync_all()?;Ok(summary)
}

pub(crate) fn verify_file(home:&Path,path:&Path,limits:Limits)->Result<Summary> {
    limits.check()?;
    let before=std::fs::symlink_metadata(path)?;
    ensure!(before.is_file() && !before.file_type().is_symlink() && before.len()<=64*1024*1024,"asset manifest file admission");
    let file=open_readonly(path)?;ensure!(same(&before,&file.metadata()?),"asset manifest changed while opening");
    let mut input=BufReader::with_capacity(64*1024,file);let summary=verify(home,&mut input,limits)?;
    ensure!(same(&before,&input.get_ref().metadata()?),"asset manifest changed during verification");unchanged(path,&before)?;Ok(summary)
}

fn hash_file(path:&Path,limits:Limits,pacer:&mut Pacer)->Result<(u64,[u8;32],Metadata)> {
    pacer.charge(4*4096)?;
    let before=std::fs::symlink_metadata(path)?;
    ensure!(before.is_file() && !before.file_type().is_symlink(),"asset entry is not a regular file");
    ensure!(before.len()<=limits.max_file_bytes,"asset exceeds per-file admission");
    let mut file=open_readonly(path)?;
    ensure!(same(&before,&file.metadata()?),"asset changed while opening");
    let mut digest=Sha256::new();let mut scratch=[0u8;64*1024];let mut bytes=0u64;
    loop {
        let read=file.read(&mut scratch)?;if read==0 {break}
        bytes=bytes.checked_add(read as u64).context("asset byte overflow")?;
        ensure!(bytes<=before.len() && bytes<=limits.max_file_bytes,"asset grew while reading");
        digest.update(&scratch[..read]);
        pacer.charge(read as u64)?;
    }
    ensure!(bytes==before.len() && same(&before,&file.metadata()?),"asset changed during read");
    unchanged(path,&before)?;Ok((bytes,digest.finalize().into(),before))
}

fn emit(output:&mut impl Write,digest:&mut Sha256,record:&Record,hashed:bool)->Result<()> {
    let mut line=serde_json::to_vec(record)?;line.push(b'\n');ensure!(line.len()<=4096,"asset manifest line budget");
    output.write_all(&line)?;if hashed {digest.update(&line)};Ok(())
}

/// Write a complete JSONL manifest to the caller's private output. Partial
/// output on failure has no Complete seal and must be retained as a failure,
/// never renamed or used as proof. Names and counts are bounded, buffers fixed.
pub(crate) fn capture(home:&Path,output:&mut impl Write,limits:Limits)->Result<Summary> {
    limits.check()?;let home_before=directory(home)?.context("asset home is missing")?;
    let mut pacer=Pacer::new(limits);let mut observed=Vec::new();let mut roots=Vec::with_capacity(4);
    let mut digest=Sha256::new();let(mut files,mut bytes)=(0u64,0u64);
    emit(output,&mut digest,&Record::Header {version:1},true)?;
    for kind in KINDS {
        let path=home.join(kind.directory());let before=directory(&path)?;
        let(mut kind_files,mut kind_bytes)=(0u64,0u64);
        if let Some(before)=&before {
            for entry in std::fs::read_dir(&path)? {
                let entry=entry?;let name=entry.file_name().into_string().map_err(|_|anyhow::anyhow!("non-UTF8 asset filename"))?;filename(&name)?;
                ensure!(files<limits.max_files,"asset file-count limit");
                let(size,sha,identity)=hash_file(&entry.path(),limits,&mut pacer)?;
                bytes=bytes.checked_add(size).context("asset total overflow")?;
                ensure!(bytes<=limits.max_total_bytes,"asset total-byte limit");
                files+=1;kind_files+=1;kind_bytes=kind_bytes.checked_add(size).context("asset directory overflow")?;
                emit(output,&mut digest,&Record::File {kind,name:name.clone(),bytes:size,sha256:hex(&sha)},true)?;
                observed.push(Observed {kind,name,identity});
            }
            unchanged(&path,before)?;
        }
        emit(output,&mut digest,&Record::Directory {kind,absent:before.is_none(),files:kind_files,bytes:kind_bytes},true)?;
        roots.push((kind,before));
    }
    recheck(home,&observed,&mut pacer)?;
    for (kind,before) in roots {
        let path=home.join(kind.directory());
        if let Some(before)=before {unchanged(&path,&before)?;} else {ensure!(directory(&path)?.is_none(),"asset directory appeared during capture");}
    }
    unchanged(home,&home_before)?;ensure!(files>=limits.min_files,"asset source below expected minimum");
    let sha:[u8;32]=digest.clone().finalize().into();
    emit(output,&mut digest,&Record::Complete {files,bytes,sha256:hex(&sha)},false)?;output.flush()?;
    Ok(Summary {files,bytes,manifest_sha256:sha})
}

fn hex(bytes:&[u8])->String {bytes.iter().map(|byte|format!("{byte:02x}")).collect()}

pub(crate) fn line(input:&mut impl BufRead)->Result<Option<(Record,Vec<u8>)>> {
    let mut bytes=Vec::new();let n=(&mut *input).take(4097).read_until(b'\n',&mut bytes)?;
    if n==0 {return Ok(None)}
    ensure!(n<=4096 && bytes.last()==Some(&b'\n'),"asset manifest line/truncation budget");
    let record:Record=serde_json::from_slice(&bytes)?;
    let mut canonical=serde_json::to_vec(&record)?;canonical.push(b'\n');
    ensure!(bytes==canonical,"asset manifest is not canonical");Ok(Some((record,bytes)))
}

/// Compare every listed byte against an independently copied asset home and
/// require the complete directory membership too. This does not copy assets.
pub(crate) fn verify(home:&Path,input:&mut impl BufRead,limits:Limits)->Result<Summary> {
    limits.check()?;let home_before=directory(home)?.context("asset target home is missing")?;
    let mut pacer=Pacer::new(limits);let mut observed=Vec::new();
    let(header,raw)=line(input)?.context("missing asset manifest header")?;
    ensure!(matches!(header,Record::Header {version:1}),"unsupported asset manifest header");
    let mut digest=Sha256::new();digest.update(&raw);
    let(mut files,mut bytes,mut index,mut kind_files,mut kind_bytes)=(0u64,0u64,0usize,0u64,0u64);
    let mut names=BTreeSet::new();let mut roots=Vec::with_capacity(4);
    for kind in KINDS {let path=home.join(kind.directory());roots.push(directory(&path)?);}
    loop {
        let(record,raw)=line(input)?.context("unfinished asset manifest")?;
        match record {
            Record::File {kind,name,bytes:expected,sha256}=>{
                ensure!(index<4 && kind==KINDS[index],"asset directory order");filename(&name)?;
                ensure!(files<limits.max_files && names.insert((kind,name.clone())),"asset duplicate/file-count limit");
                ensure!(roots[index].is_some(),"listed asset directory missing");
                let(size,sha,identity)=hash_file(&home.join(kind.directory()).join(&name),limits,&mut pacer)?;
                ensure!(size==expected && hex(&sha)==sha256,"asset content mismatch");
                bytes=bytes.checked_add(size).context("asset total overflow")?;ensure!(bytes<=limits.max_total_bytes,"asset total-byte limit");
                files+=1;kind_files+=1;kind_bytes=kind_bytes.checked_add(size).context("asset directory overflow")?;
                observed.push(Observed {kind,name,identity});
            },
            Record::Directory {kind,absent,files:expected_files,bytes:expected_bytes}=>{
                ensure!(index<4 && kind==KINDS[index] && expected_files==kind_files && expected_bytes==kind_bytes,"asset directory totals/order");
                ensure!(!absent || kind_files==0,"absent directory has assets");
                if let Some(before)=&roots[index] {
                    let path=home.join(kind.directory());let mut observed=0u64;
                    for entry in std::fs::read_dir(&path)? {
                        pacer.charge(4096)?;
                        let entry=entry?;let name=entry.file_name().into_string().map_err(|_|anyhow::anyhow!("non-UTF8 asset filename"))?;filename(&name)?;
                        ensure!(observed<limits.max_files && names.contains(&(kind,name)),"unlisted asset target entry");observed+=1;
                    }
                    ensure!(observed==kind_files,"asset target membership mismatch");unchanged(&path,before)?;
                } else {ensure!(kind_files==0,"asset directory missing");}
                index+=1;kind_files=0;kind_bytes=0;
            },
            Record::Complete {files:expected_files,bytes:expected_bytes,sha256}=>{
                let sha:[u8;32]=digest.finalize().into();
                ensure!(index==4 && files>=limits.min_files && files==expected_files && bytes==expected_bytes && hex(&sha)==sha256,"asset completion seal/totals mismatch");
                ensure!(line(input)?.is_none(),"bytes after asset completion");
                recheck(home,&observed,&mut pacer)?;
                unchanged(home,&home_before)?;
                for (kind,before) in KINDS.into_iter().zip(roots) {if let Some(before)=before {unchanged(&home.join(kind.directory()),&before)?;}}
                return Ok(Summary {files,bytes,manifest_sha256:sha});
            },
            Record::Header {..}=>bail!("repeated asset header"),
        }
        digest.update(&raw);
    }
}

#[cfg(all(test,unix))]
mod tests {
    use super::*;
    const LIMITS:Limits=Limits {max_files:16,min_files:1,max_file_bytes:1024*1024,max_total_bytes:2*1024*1024,read_bytes_per_second:32*1024*1024};

    fn assets(home:&Path) {
        std::fs::create_dir(home.join("posters")).unwrap();std::fs::create_dir(home.join("stills")).unwrap();
        std::fs::write(home.join("posters/movie_7.jpg"),[0,1,255]).unwrap();
        std::fs::write(home.join("stills/abc"),[3,4,5,6]).unwrap();
    }

    #[test]
    fn asset_manifest_verifies_actual_file_bytes_and_directory_membership() {
        let source=tempfile::tempdir().unwrap();let target=tempfile::tempdir().unwrap();assets(source.path());assets(target.path());
        let mut manifest=Vec::new();let captured=capture(source.path(),&mut manifest,LIMITS).unwrap();
        assert_eq!((captured.files,captured.bytes),(2,7));
        assert_eq!(verify(target.path(),&mut &manifest[..],LIMITS).unwrap(),captured);
        std::fs::write(target.path().join("stills/extra"),[7]).unwrap();
        assert!(verify(target.path(),&mut &manifest[..],LIMITS).is_err(),"unlisted target files cannot be silently omitted");
    }

    #[test]
    fn asset_manifest_refuses_changed_bytes_truncation_and_tail() {
        let source=tempfile::tempdir().unwrap();let target=tempfile::tempdir().unwrap();assets(source.path());assets(target.path());
        let mut manifest=Vec::new();capture(source.path(),&mut manifest,LIMITS).unwrap();
        std::fs::write(target.path().join("posters/movie_7.jpg"),[0,2,255]).unwrap();
        assert!(verify(target.path(),&mut &manifest[..],LIMITS).is_err());
        assert!(verify(source.path(),&mut &manifest[..manifest.len()-1],LIMITS).is_err());
        manifest.push(b'\n');assert!(verify(source.path(),&mut &manifest[..],LIMITS).is_err());
    }

    #[test]
    fn asset_manifest_refuses_symlinks_and_admission_overruns() {
        let source=tempfile::tempdir().unwrap();assets(source.path());
        std::os::unix::fs::symlink(source.path().join("posters/movie_7.jpg"),source.path().join("stills/link")).unwrap();
        assert!(capture(source.path(),&mut Vec::new(),LIMITS).is_err());
        std::fs::remove_file(source.path().join("stills/link")).unwrap();
        assert!(capture(source.path(),&mut Vec::new(),Limits {max_files:1,..LIMITS}).is_err());
        assert!(capture(source.path(),&mut Vec::new(),Limits {max_total_bytes:6,..LIMITS}).is_err());
        assert!(capture(source.path(),&mut Vec::new(),Limits {max_file_bytes:2,..LIMITS}).is_err());
    }

    #[test]
    fn asset_manifest_rechecks_a_file_changed_after_its_first_hash() {
        struct Change {bytes:Vec<u8>,path:std::path::PathBuf,changed:bool}
        impl Write for Change {
            fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                if !self.changed && bytes.windows(13).any(|window|window==b"\"type\":\"file\"") {
                    std::fs::write(&self.path,[9,9,9])?;self.changed=true;
                }
                Ok(bytes.len())
            }
            fn flush(&mut self)->std::io::Result<()> {Ok(())}
        }
        let source=tempfile::tempdir().unwrap();assets(source.path());
        let mut output=Change {bytes:Vec::new(),path:source.path().join("posters/movie_7.jpg"),changed:false};
        assert!(capture(source.path(),&mut output,LIMITS).is_err());
        assert!(output.changed);assert!(!String::from_utf8(output.bytes).unwrap().contains("\"type\":\"complete\""));
    }

    #[test]
    fn asset_manifest_parser_never_accepts_an_unbounded_line() {
        let target=tempfile::tempdir().unwrap();assets(target.path());
        let oversized=vec![b'x';4097];assert!(verify(target.path(),&mut &oversized[..],LIMITS).is_err());
    }

    #[test]
    fn asset_manifest_file_is_private_immutable_and_outside_the_asset_tree() {
        use std::os::unix::fs::PermissionsExt;
        let source=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();assets(source.path());
        let path=evidence.path().join("assets.jsonl");
        let summary=capture_file(source.path(),&path,LIMITS).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o600);
        assert_eq!(verify_file(source.path(),&path,LIMITS).unwrap(),summary);
        let before=std::fs::read(&path).unwrap();assert!(capture_file(source.path(),&path,LIMITS).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),before);
        let wrong=source.path().join("stills/manifest.jsonl");assert!(capture_file(source.path(),&wrong,LIMITS).is_err());assert!(!wrong.exists());
    }
}
