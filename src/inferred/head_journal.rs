//! Explicit numberless HEAD-origin records. Nothing here is an overview or a
//! normalized-header acceptance receipt.
use anyhow::{Context,Result,ensure};
use sha2::{Digest,Sha256};
use std::{fs::{File,OpenOptions},io::{Read,Write,Seek,SeekFrom},path::Path,os::{fd::{AsRawFd,FromRawFd},unix::fs::{OpenOptionsExt,MetadataExt}}};
const HEADER:u64=40;
const MAGIC:&[u8;8]=b"ATLHEAD2";
const RECORD_MAX:usize=140*1024;
const SEGMENT_BYTES:u64=16*1024*1024;
const SEGMENTS:usize=16;
#[derive(Clone,PartialEq,Eq)]
#[cfg_attr(test,derive(Debug))]
pub(super) struct Record {
    pub release:i64,pub revision:i64,pub identity:[u8;32],pub group:String,
    pub server:String,pub mid:String,pub head:Vec<u8>,pub prior:Option<(i64,i64)>,pub asked_at:i64,
}
pub(super) struct DurableHead {instance:[u8;32],sequence:u64,start:u64,end:u64,record:Record}
impl DurableHead {pub(super) fn record(&self)->&Record {&self.record}}
struct Segment {sequence:u64,bytes:u64,done:u64}
pub(super) struct Journal {
    // Open directory descriptors confine every file/rename/unlink operation.
    _root:File,_state:File,dir:File,_lock:File,file:File,catalog:[u8;32],
    instance:[u8;32],segments:Vec<Segment>,cap:u64,max_segments:usize,
}
fn cname(name:&str)->Result<std::ffi::CString> {Ok(std::ffi::CString::new(name)?)}
fn open_at(dir:&File,name:&str,flags:i32)->Result<File> {
    let linked_staging=name=="segment.next";let name=cname(name)?;
    // SAFETY: both descriptors and the terminated name remain live here.
    let fd=unsafe {libc::openat(dir.as_raw_fd(),name.as_ptr(),flags|libc::O_CLOEXEC|libc::O_NOFOLLOW|libc::O_NONBLOCK,0o600)};
    if fd<0 {return Err(std::io::Error::last_os_error().into())}
    // SAFETY: successful openat returned this new owned descriptor.
    let file=unsafe {File::from_raw_fd(fd)};let metadata=file.metadata()?;
    ensure!(metadata.is_file()&&(metadata.nlink()==1||(linked_staging&&metadata.nlink()==2))&&metadata.uid()==dir.metadata()?.uid()&&metadata.mode()&0o077==0,"HEAD journal file ownership/type changed");Ok(file)
}
fn directory_at(parent:&File,name:&str)->Result<File> {
    let name=cname(name)?;
    // SAFETY: valid borrowed directory and terminated relative name.
    if unsafe {libc::mkdirat(parent.as_raw_fd(),name.as_ptr(),0o700)}!=0 {
        let error=std::io::Error::last_os_error();if error.kind()!=std::io::ErrorKind::AlreadyExists{return Err(error.into())}
    }
    // SAFETY: open only the actual child directory, never a symlink.
    let fd=unsafe {libc::openat(parent.as_raw_fd(),name.as_ptr(),libc::O_RDONLY|libc::O_CLOEXEC|libc::O_NOFOLLOW|libc::O_DIRECTORY)};
    if fd<0{return Err(std::io::Error::last_os_error().into())}
    // SAFETY: successful openat returned this owned descriptor.
    let file=unsafe {File::from_raw_fd(fd)};let metadata=file.metadata()?;ensure!(metadata.uid()==parent.metadata()?.uid(),"HEAD journal directory owner changed");
    if name.as_bytes()==b"inferred-heads" {ensure!(metadata.mode()&0o077==0,"HEAD journal directory is not private");}
    crate::nxv_store::sync::full_flush(parent)?;Ok(file)
}
fn unlink(dir:&File,name:&str)->Result<()> {
    let name=cname(name)?;
    // SAFETY: relative name and borrowed descriptor are valid.
    if unsafe {libc::unlinkat(dir.as_raw_fd(),name.as_ptr(),0)}!=0 {let e=std::io::Error::last_os_error();if e.kind()!=std::io::ErrorKind::NotFound{return Err(e.into())}}
    Ok(())
}
fn rename(dir:&File,old:&str,new:&str)->Result<()> {
    let old=cname(old)?;let new=cname(new)?;
    // SAFETY: both names are relative to the same owned directory.
    ensure!(unsafe {libc::renameat(dir.as_raw_fd(),old.as_ptr(),dir.as_raw_fd(),new.as_ptr())}==0,std::io::Error::last_os_error());Ok(())
}
fn names(dir:&File)->Result<Vec<String>> {
    // SAFETY: fdopendir owns only this dup, never the journal's descriptor.
    let fd=unsafe {libc::dup(dir.as_raw_fd())};if fd<0{return Err(std::io::Error::last_os_error().into())}
    let pointer=unsafe {libc::fdopendir(fd)};
    if pointer.is_null(){unsafe {libc::close(fd)};return Err(std::io::Error::last_os_error().into())}
    struct Dir(*mut libc::DIR);impl Drop for Dir {fn drop(&mut self){unsafe {libc::closedir(self.0)};}}
    let stream=Dir(pointer);
    // dup shares the directory offset; each bounded inventory starts anew.
    unsafe {libc::rewinddir(stream.0)};
    let mut names=Vec::new();
    loop {
        // POSIX readdir uses errno to distinguish EOF from an actual failure.
        #[cfg(target_vendor="apple")]
        unsafe {*libc::__error()=0;}
        #[cfg(not(target_vendor="apple"))]
        unsafe {*libc::__errno_location()=0;}
        let entry=unsafe {libc::readdir(stream.0)};
        if entry.is_null(){let error=std::io::Error::last_os_error();ensure!(error.raw_os_error()==Some(0),error);break;}
        let name=unsafe {std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())}.to_str()?;
        if name=="."||name==".."{continue;}ensure!(names.len()<2*SEGMENTS+4,"HEAD journal directory exceeds entry admission");names.push(name.into());
    }Ok(names)
}
fn segment_name(sequence:u64)->String {format!("head-{sequence:016x}.log")}
fn done_name(sequence:u64)->String {format!("head-{sequence:016x}.done")}
fn field(out:&mut Vec<u8>,value:&[u8],cap:usize)->Result<()> {ensure!(value.len()<=cap,"HEAD journal field exceeds admission");out.extend_from_slice(&u32::try_from(value.len())?.to_le_bytes());out.extend_from_slice(value);Ok(())}
impl Record {
    fn encode(&self)->Result<Vec<u8>> {
        ensure!(self.release>0&&self.revision>0&&!self.group.is_empty()&&!self.server.is_empty()&&!self.mid.is_empty()&&!self.head.is_empty(),"HEAD journal source is invalid");
        let mut bytes=Vec::new();bytes.extend_from_slice(&self.release.to_le_bytes());bytes.extend_from_slice(&self.revision.to_le_bytes());bytes.extend_from_slice(&self.identity);
        bytes.push(u8::from(self.prior.is_some()));let (next,tries)=self.prior.unwrap_or((0,0));bytes.extend_from_slice(&next.to_le_bytes());bytes.extend_from_slice(&tries.to_le_bytes());bytes.extend_from_slice(&self.asked_at.to_le_bytes());
        field(&mut bytes,self.group.as_bytes(),4096)?;field(&mut bytes,self.server.as_bytes(),1024)?;field(&mut bytes,self.mid.as_bytes(),65500)?;field(&mut bytes,&self.head,65536)?;
        ensure!(bytes.len()<=RECORD_MAX,"HEAD journal record exceeds admission");Ok(bytes)
    }
    fn decode(bytes:&[u8])->Result<Self> {
        fn take<'a>(input:&mut &'a[u8],n:usize)->Result<&'a[u8]> {ensure!(input.len()>=n,"HEAD journal record is truncated");let(head,tail)=input.split_at(n);*input=tail;Ok(head)}
        fn field(input:&mut &[u8],cap:usize)->Result<Vec<u8>> {let n=u32::from_le_bytes(take(input,4)?.try_into()?) as usize;ensure!(n<=cap,"HEAD journal field exceeds admission");Ok(take(input,n)?.to_vec())}
        let mut input=bytes;let release=i64::from_le_bytes(take(&mut input,8)?.try_into()?);let revision=i64::from_le_bytes(take(&mut input,8)?.try_into()?);let identity=take(&mut input,32)?.try_into()?;
        let tag=take(&mut input,1)?[0];let next=i64::from_le_bytes(take(&mut input,8)?.try_into()?);let tries=i64::from_le_bytes(take(&mut input,8)?.try_into()?);let asked_at=i64::from_le_bytes(take(&mut input,8)?.try_into()?);
        ensure!(tag<=1&&(tag==1||(next==0&&tries==0)),"HEAD continuation shape invalid");let prior=(tag==1).then_some((next,tries));
        let group=String::from_utf8(field(&mut input,4096)?)?;let server=String::from_utf8(field(&mut input,1024)?)?;let mid=String::from_utf8(field(&mut input,65500)?)?;let head=field(&mut input,65536)?;
        ensure!(input.is_empty()&&release>0&&revision>0,"HEAD journal record shape changed");Ok(Self {release,revision,identity,group,server,mid,head,prior,asked_at})
    }
}
impl Journal {
    pub(super) fn check_root(&self,root:&Path)->Result<()> {
        let actual=std::fs::symlink_metadata(root)?;let original=self._root.metadata()?;
        ensure!(actual.is_dir()&&!actual.file_type().is_symlink()&&actual.dev()==original.dev()&&actual.ino()==original.ino(),"HEAD catalog root was rebound");Ok(())
    }
    pub(super) fn open(root:&Path,catalog:[u8;32])->Result<Self> {Self::open_limits(root,catalog,SEGMENT_BYTES,SEGMENTS)}
    fn open_limits(root:&Path,catalog:[u8;32],cap:u64,max_segments:usize)->Result<Self> {
        ensure!((256..=SEGMENT_BYTES).contains(&cap)&&(1..=SEGMENTS).contains(&max_segments),"HEAD journal bounds invalid");
        let root=OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY|libc::O_NOFOLLOW|libc::O_CLOEXEC).open(root)?;
        let state=directory_at(&root,"_state")?;let dir=directory_at(&state,"inferred-heads")?;
        let lock=open_at(&dir,"owner.lock",libc::O_RDWR|libc::O_CREAT)?;
        // A stable, content-free flock has no partial scope/key bootstrap.
        ensure!(unsafe {libc::flock(lock.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0,std::io::Error::last_os_error());
        for staging in ["segment.next","done.next"] {match open_at(&dir,staging,libc::O_RDONLY) {
            Ok(file)=>{
                if file.metadata()?.nlink()==2 {
                    let metadata=file.metadata()?;let mut matched=false;
                    for name in names(&dir)?.into_iter().filter(|name|name.starts_with("head-")&&name.ends_with(".log")) {
                        // The sole extra link must be the published segment in
                        // THIS directory, never an arbitrary hardlinked input.
                        let name=cname(&name)?;
                        let fd=unsafe {libc::openat(dir.as_raw_fd(),name.as_ptr(),libc::O_RDONLY|libc::O_CLOEXEC|libc::O_NOFOLLOW)};
                        if fd<0{return Err(std::io::Error::last_os_error().into())}
                        let canonical=unsafe {File::from_raw_fd(fd)};let candidate=canonical.metadata()?;
                        if candidate.is_file()&&candidate.dev()==metadata.dev()&&candidate.ino()==metadata.ino(){matched=true;break;}
                    }ensure!(matched,"HEAD staging link has no owned publication");
                }unlink(&dir,staging)?;
            },Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::NotFound)=>{},Err(error)=>return Err(error)
        }}
        // Rotation persists retirement intent BEFORE unlinking a completed
        // segment. Reconcile only that exact typed, catalog-bound marker.
        for name in names(&dir)?.into_iter().filter(|name|name.ends_with(".retired")) {
            let number=name.strip_prefix("head-").and_then(|n|n.strip_suffix(".retired")).context("HEAD retirement name invalid")?;
            ensure!(number.len()==16,"HEAD retirement name invalid");let sequence=u64::from_str_radix(number,16)?;
            ensure!(name==format!("head-{sequence:016x}.retired"),"HEAD retirement name invalid");
            let done=Self::read_done_named(&dir,&name,sequence,&catalog)?.context("HEAD retirement marker missing")?;
            match open_at(&dir,&segment_name(sequence),libc::O_RDONLY) {
                Ok(mut file)=>{let mut header=[0;40];file.read_exact(&mut header)?;ensure!(&header[..8]==MAGIC&&header[8..]==catalog&&file.metadata()?.len()==done,"HEAD retirement cannot remove unresolved data");unlink(&dir,&segment_name(sequence))?;},
                Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::NotFound)=>{},Err(error)=>return Err(error),
            }
            unlink(&dir,&name)?;crate::nxv_store::sync::full_flush(&dir)?;
        }
        let mut sequences=Vec::new();let mut markers=Vec::new();
        for name in names(&dir)? {
            if name=="owner.lock" {continue;}
            if let Some(rest)=name.strip_prefix("head-") {
                if let Some(number)=rest.strip_suffix(".log") {ensure!(number.len()==16,"HEAD segment name invalid");let sequence=u64::from_str_radix(number,16)?;ensure!(name==segment_name(sequence),"HEAD segment name invalid");sequences.push(sequence);continue;}
                if let Some(number)=rest.strip_suffix(".done") {markers.push(u64::from_str_radix(number,16)?);continue;}
            }anyhow::bail!("HEAD journal contains an unknown entry");
        }
        sequences.sort_unstable();ensure!(sequences.len()<=max_segments,"HEAD journal exceeds segment admission");ensure!(markers.iter().all(|n|sequences.contains(n)),"HEAD journal has an orphan completion marker");
        if sequences.is_empty(){Self::create_segment(&dir,1,&catalog)?;sequences.push(1);}
        let mut segments=Vec::new();
        for sequence in sequences {
            let mut file=open_at(&dir,&segment_name(sequence),libc::O_RDWR)?;let bytes=file.metadata()?.len();ensure!((HEADER..=cap).contains(&bytes),"HEAD segment extent invalid");
            let mut header=[0;40];file.read_exact(&mut header)?;ensure!(&header[..8]==MAGIC&&header[8..]==catalog,"HEAD journal belongs to another catalog");
            let done=Self::read_done(&dir,sequence,&catalog)?.unwrap_or(HEADER);ensure!((HEADER..=bytes).contains(&done),"HEAD completion cursor exceeds durable data");segments.push(Segment {sequence,bytes,done});
        }
        // Only the newest segment may contain one unconsumed response.
        ensure!(segments[..segments.len()-1].iter().all(|s|s.done==s.bytes),"HEAD journal has an unresolved older segment");
        let file=open_at(&dir,&segment_name(segments.last().unwrap().sequence),libc::O_RDWR)?;let mut instance=[0;32];getrandom::fill(&mut instance).map_err(|_|anyhow::anyhow!("HEAD journal attempt identity unavailable"))?;
        let mut journal=Self {_root:root,_state:state,dir,_lock:lock,file,catalog,instance,segments,cap,max_segments};let _=journal.pending()?;Ok(journal)
    }
    fn create_segment(dir:&File,sequence:u64,catalog:&[u8;32])->Result<()> {
        let mut file=open_at(dir,"segment.next",libc::O_RDWR|libc::O_CREAT|libc::O_EXCL)?;file.write_all(MAGIC)?;file.write_all(catalog)?;crate::nxv_store::sync::full_flush(&file)?;
        let old=cname("segment.next")?;let new=cname(&segment_name(sequence))?;
        // A durable complete header is published without clobbering any name.
        ensure!(unsafe {libc::linkat(dir.as_raw_fd(),old.as_ptr(),dir.as_raw_fd(),new.as_ptr(),0)}==0,std::io::Error::last_os_error());unlink(dir,"segment.next")?;crate::nxv_store::sync::full_flush(dir)?;Ok(())
    }
    fn read_done(dir:&File,sequence:u64,catalog:&[u8;32])->Result<Option<u64>> {Self::read_done_named(dir,&done_name(sequence),sequence,catalog)}
    fn read_done_named(dir:&File,name:&str,sequence:u64,catalog:&[u8;32])->Result<Option<u64>> {
        let mut file=match open_at(dir,name,libc::O_RDONLY) {Ok(file)=>file,Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::NotFound)=>return Ok(None),Err(error)=>return Err(error)};
        ensure!(file.metadata()?.len()==88,"HEAD completion record extent invalid");let mut b=[0;88];file.read_exact(&mut b)?;
        ensure!(&b[..8]==b"ATHDONE1"&&b[8..40]==*catalog&&u64::from_le_bytes(b[40..48].try_into()?)==sequence&&Sha256::digest(&b[..56]).as_slice()==&b[56..],"HEAD completion record invalid");Ok(Some(u64::from_le_bytes(b[48..56].try_into()?)))
    }
    pub(super) fn pending(&mut self)->Result<Option<DurableHead>> {
        let segment=self.segments.last_mut().unwrap();let end=self.file.metadata()?.len();
        ensure!(end>=segment.done&&end<=self.cap,"HEAD segment changed outside admission");if end==segment.done{return Ok(None)}
        self.file.seek(SeekFrom::Start(segment.done))?;
        let mut frame=[0;8];let available=end-segment.done;
        if available<8 {self.file.set_len(segment.done)?;crate::nxv_store::sync::full_flush(&self.file)?;segment.bytes=segment.done;return Ok(None)}
        self.file.read_exact(&mut frame)?;let n=u32::from_le_bytes(frame[..4].try_into()?) as usize;ensure!((64..=RECORD_MAX).contains(&n),"HEAD frame length invalid");
        if available<8+n as u64 {self.file.set_len(segment.done)?;crate::nxv_store::sync::full_flush(&self.file)?;segment.bytes=segment.done;return Ok(None)}
        ensure!(available==8+n as u64,"HEAD segment has an unaccounted tail");let mut bytes=vec![0;n];self.file.read_exact(&mut bytes)?;
        ensure!(crc32fast::hash(&bytes)==u32::from_le_bytes(frame[4..].try_into()?),"HEAD frame checksum mismatch");
        let record=Record::decode(&bytes)?;
        // Recovery can find a complete frame written BEFORE a crashed fsync.
        // Certify the actual retained bytes now before issuing a durable token.
        crate::nxv_store::sync::full_flush(&self.file)?;
        Ok(Some(DurableHead {instance:self.instance,sequence:segment.sequence,start:segment.done,end,record}))
    }
    pub(super) fn append(&mut self,record:Record)->Result<DurableHead> {
        ensure!(self.pending()?.is_none(),"HEAD journal requires actual pending-response reconciliation");let bytes=record.encode()?;let charge=8+bytes.len() as u64;ensure!(charge+HEADER<=self.cap,"HEAD record exceeds segment admission");
        if self.segments.last().unwrap().bytes+charge>self.cap {
            let sequence=self.segments.last().unwrap().sequence.checked_add(1).context("HEAD sequence overflow")?;
            if self.segments.len()==self.max_segments {
                let oldest=&self.segments[0];ensure!(oldest.done==oldest.bytes,"HEAD rotation cannot remove unresolved data");
                let retired=format!("head-{:016x}.retired",oldest.sequence);rename(&self.dir,&done_name(oldest.sequence),&retired)?;crate::nxv_store::sync::full_flush(&self.dir)?;
                unlink(&self.dir,&segment_name(oldest.sequence))?;crate::nxv_store::sync::full_flush(&self.dir)?;unlink(&self.dir,&retired)?;crate::nxv_store::sync::full_flush(&self.dir)?;self.segments.remove(0);
            }
            Self::create_segment(&self.dir,sequence,&self.catalog)?;self.file=open_at(&self.dir,&segment_name(sequence),libc::O_RDWR)?;self.segments.push(Segment {sequence,bytes:HEADER,done:HEADER});
        }
        let segment=self.segments.last_mut().unwrap();let start=segment.bytes;self.file.seek(SeekFrom::Start(start))?;self.file.write_all(&u32::try_from(bytes.len())?.to_le_bytes())?;self.file.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;self.file.write_all(&bytes)?;
        crate::nxv_store::sync::full_flush(&self.file)?;segment.bytes=start+charge;
        Ok(DurableHead {instance:self.instance,sequence:segment.sequence,start,end:segment.bytes,record})
    }
    /// Raw journal retirement is separate from PG acceptance. The service calls
    /// this only after genuine commit/reconciliation or explicit stale discard;
    /// it must never consume a record on a PG/storage error.
    pub(super) fn consumed(&mut self,record:DurableHead)->Result<()> {
        let segment=self.segments.last_mut().unwrap();ensure!(record.instance==self.instance&&record.sequence==segment.sequence&&record.start==segment.done&&record.end==segment.bytes,"HEAD retirement belongs to another record/attempt");
        let mut bytes=b"ATHDONE1".to_vec();bytes.extend_from_slice(&self.catalog);bytes.extend_from_slice(&record.sequence.to_le_bytes());bytes.extend_from_slice(&record.end.to_le_bytes());let digest=Sha256::digest(&bytes);bytes.extend_from_slice(&digest);
        let mut file=open_at(&self.dir,"done.next",libc::O_WRONLY|libc::O_CREAT|libc::O_EXCL)?;file.write_all(&bytes)?;crate::nxv_store::sync::full_flush(&file)?;rename(&self.dir,"done.next",&done_name(record.sequence))?;crate::nxv_store::sync::full_flush(&self.dir)?;segment.done=record.end;Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(n:i64)->Record {Record {release:n,revision:7,identity:[3;32],prior:None,asked_at:1,group:"example.group".into(),server:"actual.fixture.server".into(),mid:format!("part{n}of4.own@example.invalid"),head:format!("Subject: Own - \"own.bin\" yEnc ({n}/4)\r\nMessage-ID: <part{n}of4.own@example.invalid>\r\n").into_bytes()}}
    #[test]
    fn explicit_head_journal_refuses_rebinding_of_the_selected_catalog_root() {
        let parent=tempfile::tempdir().unwrap();let root=parent.path().join("selected");std::fs::create_dir(&root).unwrap();
        let journal=Journal::open(&root,[1;32]).unwrap();journal.check_root(&root).unwrap();
        std::fs::rename(&root,parent.path().join("preserved")).unwrap();let foreign=tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(foreign.path(),&root).unwrap();assert!(journal.check_root(&root).is_err());
        assert!(!foreign.path().join("_state").exists(),"rebound root never acquires this journal's writer");
    }
    #[test]
    fn explicit_head_journal_replays_exact_origin_and_bytes_before_any_acceptance() {
        let root=tempfile::tempdir().unwrap();let catalog=[1;32];
        let mut journal=Journal::open(root.path(),catalog).unwrap();let expected=record(3);
        let durable=journal.append(expected.clone()).unwrap();assert_eq!(durable.record(),&expected);drop(durable);drop(journal);
        let mut reopened=Journal::open(root.path(),catalog).unwrap();let pending=reopened.pending().unwrap().expect("real durable pending HEAD");assert_eq!(pending.record(),&expected);
        reopened.consumed(pending).unwrap();assert!(reopened.pending().unwrap().is_none());drop(reopened);
        assert!(Journal::open(root.path(),[2;32]).is_err(),"another catalog cannot adopt these raw responses");
    }
    #[test]
    fn explicit_head_journal_recovers_a_torn_unaccepted_tail_but_refuses_corrupt_complete_frames() {
        use std::io::{Seek,SeekFrom,Write};
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::open(root.path(),[1;32]).unwrap();
        let first=journal.append(record(3)).unwrap();journal.consumed(first).unwrap();
        let length=journal.file.metadata().unwrap().len();journal.file.seek(SeekFrom::End(0)).unwrap();journal.file.write_all(&[100,0,0,0,1,2]).unwrap();journal.file.sync_all().unwrap();drop(journal);
        let mut journal=Journal::open(root.path(),[1;32]).unwrap();assert!(journal.pending().unwrap().is_none());assert_eq!(journal.file.metadata().unwrap().len(),length);
        let _pending=journal.append(record(2)).unwrap();journal.file.seek(SeekFrom::End(-1)).unwrap();journal.file.write_all(b"x").unwrap();journal.file.sync_all().unwrap();drop(journal);
        assert!(Journal::open(root.path(),[1;32]).is_err(),"established CRC corruption must not become an empty/replayed response");
    }
    #[test]
    fn explicit_head_journal_rotates_only_consumed_segments_and_refuses_a_second_unaccepted_append() {
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::open_limits(root.path(),[1;32],512,2).unwrap();
        let pending=journal.append(record(3)).unwrap();assert!(journal.append(record(2)).is_err());journal.consumed(pending).unwrap();
        for n in 1..20 {let record=journal.append(record(n)).unwrap();journal.consumed(record).unwrap();}
        assert!(journal.segments.len()<=2);assert!(journal.segments.iter().all(|segment|segment.bytes<=512));
        drop(journal);assert!(Journal::open_limits(root.path(),[1;32],512,2).unwrap().pending().unwrap().is_none());
    }
}
