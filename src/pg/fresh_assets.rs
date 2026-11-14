//! Retained assets require both a real PG association and a complete admitted
//! local manifest. The manifest grants no provider access or catalog readiness.
use anyhow::{Result,Context,ensure,bail};
use std::{collections::BTreeMap,fs::File,io::{BufReader,Read,Write},path::{Path,PathBuf},sync::Arc};
use sha2::{Digest,Sha256};
use super::cache_assets::{self,Record,KINDS};
pub(crate) use super::cache_assets::Kind as AssetKind;

#[derive(Clone)]
pub(crate) struct AssetRequest {pub kind:AssetKind,pub name:String,pub digest:Option<[u8;32]>,pub bytes:Option<u64>,pub content_type:&'static str}
struct Entry {bytes:u64,digest:[u8;32]}
pub(crate) struct AssetManifest {root:File,path:PathBuf,entries:BTreeMap<(AssetKind,String),Entry>}

pub(super) fn hex(bytes:&[u8])->String {bytes.iter().map(|byte|format!("{byte:02x}")).collect()}
pub(super) fn digest(text:&str)->Result<[u8;32]> {
    ensure!(text.len()==64&&text.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)),"invalid retained asset digest");
    let mut value=[0u8;32];for (at,byte) in value.iter_mut().enumerate(){*byte=u8::from_str_radix(&text[at*2..at*2+2],16)?;}Ok(value)
}
#[cfg(unix)]
fn same_inode(a:&std::fs::Metadata,b:&std::fs::Metadata)->bool {use std::os::unix::fs::MetadataExt;a.dev()==b.dev()&&a.ino()==b.ino()}
#[cfg(not(unix))]
fn same_inode(_a:&std::fs::Metadata,_b:&std::fs::Metadata)->bool {false}
fn physical_child(parent:&File,name:&std::ffi::OsStr,directory:bool)->std::io::Result<File> {
    #[cfg(unix)] {
        use std::os::{fd::{AsRawFd,FromRawFd},unix::ffi::OsStrExt};
        let name=std::ffi::CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
        let flags=libc::O_RDONLY|libc::O_NOFOLLOW|libc::O_CLOEXEC|libc::O_NONBLOCK|if directory{libc::O_DIRECTORY}else{0};
        let fd=unsafe{libc::openat(parent.as_raw_fd(),name.as_ptr(),flags)};
        if fd<0{return Err(std::io::Error::last_os_error());}
        Ok(unsafe{File::from_raw_fd(fd)})
    }
    #[cfg(not(unix))] {let _=(parent,name,directory);Err(std::io::Error::other("retained asset confinement requires Unix"))}
}
fn physical_directory(path:&Path)->Result<File> {
    ensure!(path.is_absolute(),"retained asset root must be absolute");
    let mut current=File::open(Path::new("/"))?;
    for component in path.components(){match component {
        std::path::Component::RootDir=>{},
        std::path::Component::Normal(name)=>current=physical_child(&current,name,true)?,
        _=>bail!("retained asset root contains a relative component"),
    }}ensure!(current.metadata()?.is_dir(),"retained asset root is not a directory");Ok(current)
}
pub(super) const IMAGE_BYTES:u64=64<<20;
pub(super) const STILL_BYTES:u64=256<<20;
const IMAGE_CACHE_BYTES:u64=2<<30;
fn image_charge(length:u64)->u64 {length.div_ceil(4096).saturating_mul(4096).max(16<<10)}
#[cfg(unix)]
fn trim_images(directory:&File,limit:u64,stop:&std::sync::atomic::AtomicBool,until:std::time::Instant)->Result<bool> {
    trim_cache(directory,limit,stop,until,None)
}
#[cfg(unix)]
fn trim_cache(directory:&File,limit:u64,stop:&std::sync::atomic::AtomicBool,until:std::time::Instant,originals:Option<&AssetManifest>)->Result<bool> {
    use std::{ffi::{CStr,CString},os::fd::AsRawFd,sync::atomic::Ordering,time::SystemTime};
    struct Entries(*mut libc::DIR);
    impl Drop for Entries {fn drop(&mut self){unsafe{libc::closedir(self.0);}}}
    // readdir_r's buffer is safe only with this explicit filesystem name bound.
    // It returns an error code directly, so EOF cannot conceal an I/O error.
    let max_name=unsafe{libc::fpathconf(directory.as_raw_fd(),libc::_PC_NAME_MAX)};
    let mut entry:libc::dirent=unsafe{std::mem::zeroed()};ensure!(max_name>=0&&(max_name as usize)<entry.d_name.len(),"native image directory name extent unsupported");
    let fd=unsafe{libc::fcntl(directory.as_raw_fd(),libc::F_DUPFD_CLOEXEC,0)};if fd<0{return Err(std::io::Error::last_os_error().into())}
    let raw=unsafe{libc::fdopendir(fd)};if raw.is_null(){let error=std::io::Error::last_os_error();unsafe{libc::close(fd)};return Err(error.into())}let entries=Entries(raw);unsafe{libc::rewinddir(entries.0)};
    struct Item {at:SystemTime,name:String,charge:u64,identity:std::fs::Metadata}
    let mut items=Vec::new();let mut total=0u64;
    loop {
        if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
        if originals.is_some()&&std::time::Instant::now()>=until{return Ok(false)}
        ensure!(std::time::Instant::now()<until,"native image cache scan deadline expired");
        let mut found=std::ptr::null_mut();let error=unsafe{libc::readdir_r(entries.0,&mut entry,&mut found)};
        if error!=0{return Err(std::io::Error::from_raw_os_error(error).into())}if found.is_null(){break;}
        let name=unsafe{CStr::from_ptr(entry.d_name.as_ptr())}.to_str()?;if matches!(name,"."|".."){continue;}cache_assets::filename(name)?;
        if originals.is_some()&&items.len()>=IMAGE_CACHE_BYTES as usize/(16<<10)+64{return Ok(false)}
        ensure!(items.len()<IMAGE_CACHE_BYTES as usize/(16<<10)+64,"native image cache file-count admission");
        let file=physical_child(directory,std::ffi::OsStr::new(name),false)?;let identity=file.metadata()?;ensure!(identity.is_file(),"native image cache contains a non-file");
        let charge=image_charge(identity.len());total=total.checked_add(charge).context("native image cache charge overflow")?;
        items.push(Item {at:identity.modified()?,name:name.into(),charge,identity});
    }
    items.sort_by(|a,b|a.at.cmp(&b.at).then_with(||a.name.cmp(&b.name)));
    for item in items {
        if total<=limit{break;}
        if originals.is_some_and(|manifest|manifest.entries.contains_key(&(AssetKind::Stills,item.name.clone()))) {continue;}
        if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
        if originals.is_some()&&std::time::Instant::now()>=until{return Ok(false)}
        ensure!(std::time::Instant::now()<until,"native image cache trim deadline expired");
        let file=match physical_child(directory,std::ffi::OsStr::new(&item.name),false){Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{total-=item.charge;continue},Err(error)=>return Err(error.into())};
        if originals.is_some() {
            use std::os::fd::AsRawFd;
            if unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}<0 {
                let error=std::io::Error::last_os_error();if error.kind()==std::io::ErrorKind::WouldBlock{continue;}return Err(error.into())
            }
        }
        let current=file.metadata()?;ensure!(same_inode(&item.identity,&current)&&item.identity.len()==current.len()&&item.identity.modified()?==current.modified()?,"native image cache entry changed during trim");
        let name=CString::new(item.name)?;if unsafe{libc::unlinkat(directory.as_raw_fd(),name.as_ptr(),0)}==0{total-=item.charge;}
    }
    Ok(total<=limit)
}
pub(super) struct StillFile {file:File,before:std::fs::Metadata}
pub(super) struct Receipt {bytes:i64,digest:Vec<u8>,source:Vec<u8>}
pub(super) fn receipt_in(client:&mut impl postgres::GenericClient,request:&AssetRequest)->Result<Option<Receipt>> {
    cache_assets::filename(&request.name)?;
    client.query_opt("select byte_length,digest,source_digest from ext_asset_files where kind=$1 and name=$2",&[&request.kind.directory(),&request.name])?
        .map(|row|Ok(Receipt {bytes:row.try_get(0)?,digest:row.try_get(1)?,source:row.try_get(2)?})).transpose()
}
impl AssetManifest {
    pub(crate) fn from_selection(selection:&crate::config::NativeAssetsConfig)->Result<Self> {
        Self::open(Path::new(&selection.root),Path::new(&selection.manifest),digest(&selection.whole_sha256)?)
    }
    pub(super) fn namespace(&self)->Result<[u8;32]> {
        self.check_root()?;
        #[cfg(unix)] {use std::os::unix::fs::MetadataExt;
            let metadata=self.root.metadata()?;let mut digest=Sha256::new();
            digest.update(b"atlas.asset-physical-namespace.v1\0");digest.update(metadata.dev().to_le_bytes());digest.update(metadata.ino().to_le_bytes());
            Ok(digest.finalize().into())
        }
        #[cfg(not(unix))] {bail!("asset namespaces require Unix")}
    }
    pub(super) fn native_source(&self,kind:AssetKind,catalog:&super::stage::CatalogIdentity)->Result<[u8;32]> {
        let domain=match kind {AssetKind::Covers=>b"atlas.native-cover-writer.v1\0".as_slice(),AssetKind::Posters=>b"atlas.native-poster-writer.v1\0".as_slice(),AssetKind::Images=>b"atlas.native-image-writer.v1\0".as_slice(),AssetKind::Stills=>b"atlas.native-still-writer.v1\0".as_slice()};
        let mut digest=Sha256::new();digest.update(domain);digest.update(self.namespace()?);digest.update(catalog.nonce());Ok(digest.finalize().into())
    }
    pub(super) fn read_associated(&self,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>,catalog:&super::stage::CatalogIdentity,receipt:Option<&Receipt>)->Result<Option<crate::nzb_spool::Spool>> {
        let limit=match request.kind {AssetKind::Covers=>crate::probe::media::MAX_COVER as u64,AssetKind::Posters=>8<<20,_=>256<<20};
        if let Some(entry)=self.entries.get(&(request.kind,request.name.clone())) {ensure!(entry.bytes<=limit,"asset exceeds family byte admission");}
        let Some(receipt)=receipt else{return self.read(request,pool)};
        ensure!(receipt.bytes>=0&&receipt.bytes as u64<=limit,"asset receipt exceeds family byte admission");
        ensure!((0..=256*1024*1024).contains(&receipt.bytes)&&receipt.digest.len()==32&&receipt.source.len()==32,"asset receipt admission mismatch");
        let hash:[u8;32]=receipt.digest.as_slice().try_into()?;
        if let Some(expected)=request.digest {ensure!(expected==hash,"PG asset association and writer descriptor disagree");}
        if let Some(expected)=request.bytes {ensure!(expected==receipt.bytes as u64,"PG asset association and writer length disagree");}
        let checked=AssetRequest {digest:Some(hash),bytes:Some(receipt.bytes as u64),..request.clone()};
        if self.entries.get(&(request.kind,request.name.clone())).is_some_and(|entry|entry.digest==hash&&entry.bytes==receipt.bytes as u64) {return self.read(&checked,pool)}
        ensure!(receipt.source.as_slice()==self.native_source(request.kind,catalog)?.as_slice(),"dynamic asset writer namespace mismatch");
        self.read_recorded(&checked,pool)
    }
    /// Actual PG row and sealed catalog identity are read on the caller's
    /// existing read transaction; no public request can supply writer evidence.
    pub(crate) fn read_native(&self,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>,client:&mut impl postgres::GenericClient)->Result<Option<crate::nzb_spool::Spool>> {
        let receipt=receipt_in(client,request)?;
        if receipt.is_none() {return self.read(request,pool)}
        let catalog=super::stage::CatalogIdentity::read_in(client)?;
        self.read_associated(request,pool,&catalog,receipt.as_ref())
    }
    pub(super) fn check_root(&self)->Result<()> {
        let current=physical_directory(&self.path)?;
        ensure!(same_inode(&self.root.metadata()?,&current.metadata()?),"retained asset root identity changed");Ok(())
    }

    /// `expected_file_sha256` is the separately admitted WHOLE manifest file
    /// hash, distinct from Complete.sha256 (the canonical prefix hash). Root
    /// selection is explicit; no HTTP field or persisted name chooses it.
    pub(crate) fn open(root:&Path,manifest:&Path,expected_file_sha256:[u8;32])->Result<Self> {
        let root_handle=physical_directory(root)?;
        let parent=physical_directory(manifest.parent().context("retained manifest parent missing")?)?;
        let file=physical_child(&parent,manifest.file_name().context("retained manifest name missing")?,false)?;
        let before=file.metadata()?;ensure!(before.is_file()&&before.len()<=64*1024*1024,"retained manifest extent admission");
        let mut input=BufReader::with_capacity(8192,file);let mut whole=Sha256::new();let mut prefix=Sha256::new();
        let (header,raw)=cache_assets::line(&mut input)?.context("retained manifest header missing")?;
        ensure!(matches!(header,Record::Header {version:1}),"retained manifest version");whole.update(&raw);prefix.update(&raw);
        let(mut index,mut files,mut bytes,mut directory_files,mut directory_bytes,mut wire)=(0usize,0u64,0u64,0u64,0u64,raw.len() as u64);
        let mut entries=BTreeMap::new();
        loop {
            let (record,raw)=cache_assets::line(&mut input)?.context("unfinished retained manifest")?;
            wire=wire.checked_add(raw.len() as u64).context("retained manifest extent overflow")?;
            ensure!(wire<=before.len(),"retained manifest changed while reading");whole.update(&raw);
            match record {
                Record::File {kind,name,bytes:size,sha256}=>{
                    ensure!(index<4&&kind==KINDS[index]&&files<100_000&&size<=256*1024*1024,"retained manifest file admission/order");cache_assets::filename(&name)?;
                    ensure!(entries.insert((kind,name),Entry {bytes:size,digest:digest(&sha256)?}).is_none(),"duplicate retained asset");
                    files+=1;directory_files+=1;bytes=bytes.checked_add(size).context("retained asset total overflow")?;
                    directory_bytes=directory_bytes.checked_add(size).context("retained asset directory overflow")?;ensure!(bytes<=64*1024*1024*1024,"retained asset total admission");
                }
                Record::Directory {kind,absent,files:count,bytes:size}=>{
                    ensure!(index<4&&kind==KINDS[index]&&count==directory_files&&size==directory_bytes&&(!absent||count==0),"retained asset directory count/order");
                    index+=1;directory_files=0;directory_bytes=0;
                }
                Record::Complete {files:count,bytes:size,sha256}=>{
                    let observed:[u8;32]=prefix.finalize().into();
                    ensure!(index==4&&count==files&&size==bytes&&digest(&sha256)?==observed,"retained asset completion mismatch");
                    ensure!(cache_assets::line(&mut input)?.is_none()&&wire==before.len(),"retained manifest trailing bytes");
                    let after=input.get_ref().metadata()?;
                    ensure!(same_inode(&before,&after)&&before.len()==after.len()&&before.modified()?==after.modified()?,"retained manifest changed during admission");
                    ensure!(<[u8;32]>::from(whole.finalize())==expected_file_sha256,"retained manifest file digest mismatch");
                    return Ok(Self {root:root_handle,path:root.to_path_buf(),entries});
                }
                _=>bail!("unexpected retained manifest header"),
            }
            prefix.update(&raw);
        }
    }
    pub(crate) fn read(&self,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>)->Result<Option<crate::nzb_spool::Spool>> {
        cache_assets::filename(&request.name)?;
        let Some(entry)=self.entries.get(&(request.kind,request.name.clone())) else{return Ok(None)};
        if let Some(expected)=request.digest{ensure!(expected==entry.digest,"PG asset and retained manifest disagree");}
        if let Some(expected)=request.bytes{ensure!(expected==entry.bytes,"PG asset and retained manifest lengths disagree");}
        self.copy_descriptor(request,entry.bytes,entry.digest,pool)
    }
    /// Only a checked PG writer-receipt consumer may use an unlisted descriptor.
    /// The file confinement/hash/copy operation is identical to manifest reads.
    pub(super) fn has_entry(&self,request:&AssetRequest)->Result<bool> {
        cache_assets::filename(&request.name)?;Ok(self.entries.contains_key(&(request.kind,request.name.clone())))
    }
    pub(super) fn contains_cover(&self,name:&str)->Result<bool> {cache_assets::filename(name)?;Ok(self.entries.contains_key(&(AssetKind::Covers,name.into())))}
    pub(super) fn read_cover(&self,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>)->Result<Option<crate::nzb_spool::Spool>> {
        if let Some(entry)=self.entries.get(&(AssetKind::Covers,request.name.clone())) {ensure!(entry.bytes<=crate::probe::media::MAX_COVER as u64,"retained cover exceeds worker admission");}
        self.read(request,pool)
    }
    pub(super) fn read_recorded(&self,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>)->Result<Option<crate::nzb_spool::Spool>> {
        self.copy_descriptor(request,request.bytes.context("recorded asset length missing")?,request.digest.context("recorded asset digest missing")?,pool)
    }
    fn copy_descriptor(&self,request:&AssetRequest,length:u64,expected:[u8;32],pool:&Arc<crate::nzb_spool::Pool>)->Result<Option<crate::nzb_spool::Spool>> {
        cache_assets::filename(&request.name)?;
        self.check_root()?;
        let directory=match physical_child(&self.root,std::ffi::OsStr::new(request.kind.directory()),true) {
            Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.into()),
        };
        let mut file=match physical_child(&directory,std::ffi::OsStr::new(&request.name),false) {
            Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.into()),
        };
        let before=file.metadata()?;ensure!(before.is_file()&&before.len()==length,"retained asset length/type mismatch");
        let mut output=pool.open()?;let mut hash=Sha256::new();let mut bytes=0u64;let mut scratch=[0u8;64*1024];
        let until=std::time::Instant::now()+std::time::Duration::from_secs(10);
        loop {ensure!(std::time::Instant::now()<until,"retained asset read deadline expired");let count=file.read(&mut scratch)?;if count==0{break;}bytes=bytes.checked_add(count as u64).context("retained asset read overflow")?;
            ensure!(bytes<=length,"retained asset grew during read");hash.update(&scratch[..count]);output.write_all(&scratch[..count])?;}
        let after=file.metadata()?;
        ensure!(bytes==length&&same_inode(&before,&after)&&before.len()==after.len()&&before.modified()?==after.modified()?,"retained asset changed during read");
        ensure!(<[u8;32]>::from(hash.finalize())==expected,"retained asset content mismatch");
        Ok(Some(output.finish()?))
    }
    pub(super) fn store_cover(&self,bytes:&[u8])->Result<(String,[u8;32])> {
        ensure!(!bytes.is_empty()&&bytes.len()<=crate::probe::media::MAX_COVER,"native cover exceeds byte admission");
        let name=hex(&Sha256::digest(bytes));let value=self.store_named(AssetKind::Covers,&name,bytes,crate::probe::media::MAX_COVER as u64)?;Ok((name,value))
    }
    pub(super) fn store_poster(&self,name:&str,bytes:&[u8])->Result<[u8;32]> {
        ensure!(bytes.starts_with(&[0xff,0xd8,0xff]),"native poster is not the existing JPEG representation");
        self.store_named(AssetKind::Posters,name,bytes,8<<20)
    }
    fn store_named(&self,kind:AssetKind,name:&str,bytes:&[u8],limit:u64)->Result<[u8;32]> {
        cache_assets::filename(name)?;ensure!(!bytes.is_empty()&&bytes.len() as u64<=limit,"native asset exceeds byte admission");self.check_root()?;
        #[cfg(unix)] {
            let directory=self.writable_directory(kind)?;Ok(self.store_in_directory(&directory,name,bytes)?.0)
        }
        #[cfg(not(unix))] {let _=(kind,bytes);bail!("native cover confinement requires Unix")}
    }
    #[cfg(unix)]
    fn writable_directory(&self,kind:AssetKind)->Result<File> {
        use std::os::fd::AsRawFd;
        let name=std::ffi::CString::new(kind.directory())?;
        if unsafe{libc::mkdirat(self.root.as_raw_fd(),name.as_ptr(),0o700)}<0 {
            let error=std::io::Error::last_os_error();if error.kind()!=std::io::ErrorKind::AlreadyExists{return Err(error.into())}
        }
        Ok(physical_child(&self.root,std::ffi::OsStr::new(kind.directory()),true)?)
    }
    #[cfg(unix)]
    fn store_in_directory(&self,directory:&File,name:&str,bytes:&[u8])->Result<([u8;32],bool)> {
        use std::os::fd::{AsRawFd,FromRawFd};
        let value:[u8;32]=Sha256::digest(bytes).into();let final_name=std::ffi::CString::new(name)?;
        let mut nonce=[0u8;16];getrandom::fill(&mut nonce).map_err(|_|anyhow::anyhow!("native cover entropy unavailable"))?;
        let temporary=std::ffi::CString::new(format!(".atlas-cover-{}",hex(&nonce)))?;
        let fd=unsafe{libc::openat(directory.as_raw_fd(),temporary.as_ptr(),libc::O_WRONLY|libc::O_CREAT|libc::O_EXCL|libc::O_NOFOLLOW|libc::O_CLOEXEC,0o600)};
        if fd<0{return Err(std::io::Error::last_os_error().into())}
        struct Pending<'a> {directory:&'a File,name:std::ffi::CString}
        impl Drop for Pending<'_> {fn drop(&mut self){unsafe{libc::unlinkat(self.directory.as_raw_fd(),self.name.as_ptr(),0);}}}
        let pending=Pending {directory,name:temporary};let mut file=unsafe{File::from_raw_fd(fd)};file.write_all(bytes)?;file.sync_all()?;
        let created=if unsafe{libc::linkat(directory.as_raw_fd(),pending.name.as_ptr(),directory.as_raw_fd(),final_name.as_ptr(),0)}<0 {
            let error=std::io::Error::last_os_error();if error.kind()!=std::io::ErrorKind::AlreadyExists{return Err(error.into())}false
        }else{true};
        drop(pending);directory.sync_all()?;self.root.sync_all()?;self.check_root()?;Ok((value,created))
    }
    /// Pin a named still against our cache trimmer before verifying its bytes.
    /// The returned physical handle remains with Prepared until settlement.
    pub(super) fn hold_still(&self,name:&str)->Result<Option<StillFile>> {
        digest(name)?;self.check_root()?;
        let directory=match physical_child(&self.root,std::ffi::OsStr::new("stills"),true) {Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.into())};
        let file=match physical_child(&directory,std::ffi::OsStr::new(name),false) {Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.into())};
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            if unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_SH|libc::LOCK_NB)}<0 {
                let error=std::io::Error::last_os_error();if error.kind()==std::io::ErrorKind::WouldBlock{return Ok(None)}return Err(error.into())
            }
        }
        let held=StillFile {before:file.metadata()?,file};
        // The trimmer may have unlinked this inode between open and SH flock.
        // At establishment that is ordinary unavailability, not corruption of
        // a previously admitted Prepared hold. Recheck the root independently.
        self.check_root()?;
        let current=match physical_child(&directory,std::ffi::OsStr::new(name),false) {Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.into())};
        if !same_inode(&held.before,&current.metadata()?){return Ok(None)}
        self.check_still_hold(name,&held)?;Ok(Some(held))
    }
    pub(super) fn check_still_hold(&self,name:&str,held:&StillFile)->Result<()> {
        self.check_root()?;let directory=physical_child(&self.root,std::ffi::OsStr::new("stills"),true)?;
        let current=physical_child(&directory,std::ffi::OsStr::new(name),false)?;
        let before=&held.before;let file=held.file.metadata()?;let after=current.metadata()?;
        ensure!(before.is_file()&&before.len()>0&&before.len()<=STILL_BYTES&&same_inode(before,&after)&&same_inode(before,&file)
            &&before.len()==after.len()&&before.len()==file.len()&&before.modified()?==after.modified()?&&before.modified()?==file.modified()?,"native still hold changed before settlement");Ok(())
    }

    /// The old still cache's 2GiB target and first/every64 write cadence,
    /// confined to the selected namespace. Captured originals are never evicted.
    pub(super) fn store_still(&self,bytes:&[u8],writes:&std::sync::atomic::AtomicU64,stop:&std::sync::atomic::AtomicBool)->Result<Option<(String,[u8;32])>> {
        ensure!(!bytes.is_empty()&&bytes.len() as u64<=STILL_BYTES,"native still exceeds byte admission");self.check_root()?;
        #[cfg(unix)] {
            use std::{os::fd::AsRawFd,sync::atomic::Ordering,time::{Duration,Instant}};
            let directory=self.writable_directory(AssetKind::Stills)?;let until=Instant::now()+Duration::from_secs(10);
            loop {
                if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
                if Instant::now()>=until{return Ok(None)}
                if unsafe{libc::flock(directory.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0{break;}
                let error=std::io::Error::last_os_error();if error.kind()!=std::io::ErrorKind::WouldBlock{return Err(error.into())}std::thread::sleep(Duration::from_millis(10));
            }
            let name=hex(&Sha256::digest(bytes));
            let absent=match physical_child(&directory,std::ffi::OsStr::new(&name),false) {Ok(_)=>false,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>true,Err(error)=>return Err(error.into())};
            if absent&&writes.load(Ordering::Relaxed).is_multiple_of(64)
                && !trim_cache(&directory,IMAGE_CACHE_BYTES-image_charge(bytes.len() as u64),stop,until,Some(self))? {return Ok(None)}
            let (digest,created)=self.store_in_directory(&directory,&name,bytes)?;if created{writes.fetch_add(1,Ordering::Relaxed);}
            if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
            let current=physical_child(&self.root,std::ffi::OsStr::new("stills"),true)?;
            ensure!(same_inode(&directory.metadata()?,&current.metadata()?),"native still directory changed during write");
            directory.sync_all()?;self.check_root()?;Ok(Some((name,digest)))
        }
        #[cfg(not(unix))] {let _=(writes,stop);bail!("native still confinement requires Unix")}
    }

    /// The old image cache target/cadence/charge, with every operation confined
    /// to the opened directory. No selected manifest is rewritten by eviction.
    pub(super) fn store_image(&self,bytes:&[u8],writes:&std::sync::atomic::AtomicU64,stop:&std::sync::atomic::AtomicBool)->Result<Option<(String,[u8;32])>> {
        ensure!(!bytes.is_empty()&&bytes.len() as u64<=IMAGE_BYTES,"native image exceeds byte admission");self.check_root()?;
        #[cfg(unix)] {
            use std::{os::fd::AsRawFd,sync::atomic::Ordering,time::{Duration,Instant}};
            let directory=self.writable_directory(AssetKind::Images)?;let until=Instant::now()+Duration::from_secs(10);
            loop {
                if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
                ensure!(Instant::now()<until,"native image cache lock deadline expired");
                if unsafe{libc::flock(directory.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0{break;}
                let error=std::io::Error::last_os_error();if error.kind()!=std::io::ErrorKind::WouldBlock{return Err(error.into())}std::thread::sleep(Duration::from_millis(10));
            }
            let name=hex(&Sha256::digest(bytes));
            let absent=match physical_child(&directory,std::ffi::OsStr::new(&name),false) {Ok(_)=>false,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>true,Err(error)=>return Err(error.into())};
            if absent&&writes.load(Ordering::Relaxed).is_multiple_of(64) {
                // Reserve the incoming file on the same first/every64 turn.
                // A full or malformed cache cannot accumulate another orphan
                // on each retry, and the just-produced file is not evicted.
                match trim_images(&directory,IMAGE_CACHE_BYTES-image_charge(bytes.len() as u64),stop,until) {
                    Ok(true)=>{},Ok(false)=>return Ok(None),Err(error)=>return Err(error)
                }
            }
            let (value,created)=self.store_in_directory(&directory,&name,bytes)?;if created{writes.fetch_add(1,Ordering::Relaxed);}
            if stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into())}
            let current=physical_child(&self.root,std::ffi::OsStr::new("images"),true)?;
            ensure!(same_inode(&directory.metadata()?,&current.metadata()?),"native image directory changed during write");
            directory.sync_all()?;self.check_root()?;Ok(Some((name,value)))
        }
        #[cfg(not(unix))] {let _=(writes,stop);bail!("native image confinement requires Unix")}
    }

}

#[cfg(test)]
#[path="fresh_asset_tests.rs"]
mod tests;

impl super::fresh_reader::ReadView<'_> {
    pub(crate) fn read_asset(&self,manifest:&AssetManifest,request:&AssetRequest,pool:&Arc<crate::nzb_spool::Pool>)->Result<Option<crate::nzb_spool::Spool>> {
        self.before_query()?;manifest.read_native(request,pool,&mut *self.tx.borrow_mut())
    }
    pub(crate) fn asset_request(&self,path:&str,args:&std::collections::HashMap<String,String>)->Result<Option<AssetRequest>> {
        let number=|key:&str|args.get(key).and_then(|n|n.trim().parse::<i64>().ok()).unwrap_or(0);
        let (kind,name,mut sha,content_type)=if path=="/ui/poster" {
            let field=if args.get("art").is_some_and(|art|art=="backdrop"){"backdrop"}else{"poster"};
            self.before_query()?;
            let extent=self.tx.borrow_mut().query_opt(&format!("select octet_length({field})::bigint from ext_media_details where kind=$1 and tmdb_id=$2 and {field} is not null"),&[&number("kind"),&number("tmdb")])?;
            let Some(extent)=extent else{return Ok(None)};let length:i64=extent.try_get(0)?;ensure!(length>=0,"negative native artwork name extent");
            self.admit_owned((length as u64).checked_mul(3).and_then(|n|n.checked_add(512)).context("native artwork name extent overflow")?)?;self.before_query()?;
            let value:String=self.tx.borrow_mut().query_one(&format!("select {field} from ext_media_details where kind=$1 and tmdb_id=$2"),&[&number("kind"),&number("tmdb")])?.try_get(0)?;
            let name=crate::media::details::kept_image_name(&value).context("invalid native artwork cache name")?;
            (AssetKind::Posters,name,None,"image/jpeg")
        }else {
            let id=number("id");if !self.visible(id)?{return Ok(None);}
            let (table,extra,at,kind)=match path {
                "/ui/cover"=>("release_cover","",0,AssetKind::Covers),
                "/ui/still"=>{let at=number("at");if u8::try_from(at).is_err(){return Ok(None);}("release_stills"," and position=$2",at,AssetKind::Stills)},
                "/ui/image"=>("release_images"," and position=$2",number("at"),AssetKind::Images),
                _=>bail!("unknown native asset route"),
            };
            let mime=match path {"/ui/cover"=>"mime","/ui/image"=>"format",_=>"'jpeg'"};
            self.before_query()?;
            let sql=format!("select octet_length(sha256)::bigint,octet_length({mime})::bigint from {table} where release_id=$1{extra}");
            let params:Vec<&(dyn postgres::types::ToSql+Sync)>=if extra.is_empty(){vec![&id]}else{vec![&id,&at]};
            let Some(extent)=self.tx.borrow_mut().query_opt(&sql,&params)? else{return Ok(None)};
            ensure!(extent.try_get::<_,i64>(0)?==64&&(0..=64).contains(&extent.try_get::<_,i64>(1)?),"invalid native asset descriptor extent");
            self.before_query()?;
            let row=self.tx.borrow_mut().query_one(&format!("select sha256,{mime} from {table} where release_id=$1{extra}"),&params)?;
            let sha:String=row.try_get(0)?;let format:String=row.try_get(1)?;let value=digest(&sha)?;
            let content_type=if path=="/ui/cover"{if format=="image/png"{"image/png"}else{"image/jpeg"}}
                else if path=="/ui/still"{"image/jpeg"}else{crate::images::content_type(&format)};
            (kind,sha,Some(value),content_type)
        };
        cache_assets::filename(&name)?;
        // Where the optional imported descriptor exists, it must agree with
        // the actual association. It is never used as a substitute for the
        // complete root-bound manifest or the per-read file content check.
        self.before_query()?;
        let mut length=None;
        if let Some(row)=self.tx.borrow_mut().query_opt("select byte_length,digest,source_digest from ext_asset_files where kind=$1 and name=$2",&[&kind.directory(),&name])? {
            length=Some(u64::try_from(row.try_get::<_,i64>(0)?).context("negative imported asset length")?);
            let bytes:Vec<u8>=row.try_get(1)?;let source:Vec<u8>=row.try_get(2)?;ensure!(bytes.len()==32&&source.len()==32,"invalid imported asset descriptor");
            if let Some(expected)=sha{ensure!(bytes.as_slice()==expected,"imported asset association mismatch");}
            sha=Some(bytes.try_into().map_err(|_|anyhow::anyhow!("imported asset digest changed"))?);
        }
        Ok(Some(AssetRequest {kind,name,digest:sha,bytes:length,content_type}))
    }
}

#[cfg(all(test,unix))]
#[path="fresh_image_asset_tests.rs"]
mod image_cache_tests;
