//! Borrowed NZB-to-writer conversion. The caller holds the original native job
//! admission BEFORE next_batch allocates expanded Articles, and drops each
//! checked batch before building another. No full expanded Release is retained.
use anyhow::{Result,ensure};
use crate::{archive::nzb::{Parsed,NzbFile},parser::{Article,Release,parse_subject}};
use super::{full::FullSpot,release};

struct File<'a> {
    source:&'a NzbFile,order:Vec<usize>,filename:String,author:&'a str,date:String,
    index:i64,total:i64,parts:i64,
}
pub(super) struct Plan<'a> {
    files:Vec<File<'a>>,metadata:Release,file:usize,segment:usize,key:String,
}
impl<'a> Plan<'a> {
    pub(super) fn new(spot:&'a FullSpot,nzb:&'a Parsed)->Result<Option<Self>> {
        if !nzb.complete || nzb.damaged || nzb.left_out>0 || nzb.files.is_empty()
            || nzb.files.iter().any(|file|release::allowed(file).is_none()
                || file.segments.len()!=file.articles || file.segments.iter().any(|segment|segment.message_id.is_empty())) {
            return Ok(None)
        }
        let segments=nzb.files.iter().try_fold(0usize,|n,file|n.checked_add(file.segments.len()));
        if segments.is_none_or(|count|count==0 || count>release::MAX_SEGMENTS) {return Ok(None)}
        let Some(title)=release::clean_title(&spot.title) else {return Ok(None)};
        let group=release::allowed(&nzb.files[0]).expect("all groups checked").clone();
        let total=i64::try_from(nzb.files.len())?;let mut files=Vec::with_capacity(nzb.files.len());let mut size=0i64;
        for (at,file) in nzb.files.iter().enumerate() {
            let subject=parse_subject(&file.subject);
            let filename=subject.as_ref().map(|parsed|parsed.filename.clone()).filter(|name|!name.is_empty())
                .unwrap_or_else(||if file.name.is_empty() {format!("{title}.{:03}",at+1)} else {file.name.clone()});
            let (index,total)=subject.as_ref().and_then(|parsed|Some((parsed.file_index?,parsed.file_total?))).unwrap_or((at as i64+1,total));
            let mut order=(0..file.segments.len()).collect::<Vec<_>>();order.sort_by_key(|&at|file.segments[at].number);
            let parts=(order.len() as i64).max(subject.as_ref().map_or(0,|parsed|parsed.total_parts));
            for segment in &file.segments {
                size=size.checked_add(i64::try_from(segment.bytes).unwrap_or(i64::MAX))
                    .ok_or_else(||anyhow::anyhow!("Spotnet NZB byte total exceeds native integer admission"))?;
            }
            files.push(File {source:file,order,filename,author:if file.poster.is_empty(){&spot.poster}else{&file.poster},
                date:release::iso_date(if file.date>0 {file.date}else{spot.created}),index,total,parts});
        }
        let first=files.iter().find(|file|!file.order.is_empty()).expect("at least one segment");
        // The native writer derives completeness from actual committed parts;
        // this untrusted input hint is never a publication/readiness fact.
        let metadata=Release {name:title,group,poster:first.author.to_owned(),date:first.date.clone(),size,
            complete:false,is_obfuscated:false,display_name:None,collection:None,articles:Vec::new()};
        Ok(Some(Self {files,metadata,file:0,segment:0,key:release::nzb_key(nzb)}))
    }
    pub(super) fn key(&self)->&str {&self.key}
    pub(super) fn metadata(&self)->&Release {&self.metadata}
    pub(super) fn junk_marker(&self)->Option<Release> {
        let filename=self.files.iter().find(|file|crate::junk::is_executable(&file.filename))?.filename.clone();
        let mut marker=self.metadata.clone();
        marker.articles=vec![Article {filename:Some(filename),..Default::default()}];Some(marker)
    }
    pub(super) fn rename(&mut self,name:&str) {self.metadata.name=name.to_owned();}
    /// Iterate borrowed expected filename/MID pairs without expanding Articles.
    pub(super) fn identities(&self)->impl Iterator<Item=(&str,&str)> {
        self.files.iter().flat_map(|file|file.order.iter().map(move |&at|(file.filename.as_str(),file.source.segments[at].message_id.as_str())))
    }
    pub(super) fn next_batch(&mut self,limit:usize)->Result<Option<Release>> {
        let mut batch=self.metadata.clone();let base=release_heap(&batch);
        ensure!(base<limit,"Spotnet metadata exceeds actual native job admission");let mut strings=0usize;
        while self.file<self.files.len() {
            let file=&self.files[self.file];
            if self.segment==file.order.len() {self.file+=1;self.segment=0;continue}
            let segment=&file.source.segments[file.order[self.segment]];
            // Reserve for the actual next Vec capacity AND conservative String
            // capacities before copying any expanded article field.
            let capacity=if batch.articles.len()<batch.articles.capacity() {batch.articles.capacity()}
                else {batch.articles.capacity().saturating_mul(2).max(1)};
            let heap=[file.source.subject.len(),file.author.len(),file.date.len(),segment.message_id.len()+2,
                file.filename.len(),batch.name.len()].into_iter().fold(96usize,|sum,n|sum.saturating_add(n.saturating_mul(2)));
            let projected=base.saturating_add(strings).saturating_add(heap)
                .saturating_add(capacity.saturating_mul(std::mem::size_of::<Article>()));
            if projected>limit {
                ensure!(!batch.articles.is_empty(),"one Spotnet article exceeds actual native job admission");break
            }
            if capacity>batch.articles.capacity() {batch.articles.reserve_exact(capacity-batch.articles.len());}
            ensure!(base.saturating_add(strings).saturating_add(heap)
                .saturating_add(batch.articles.capacity().saturating_mul(std::mem::size_of::<Article>()))<=limit,
                "Spotnet Vec allocation exceeds actual native job admission");
            let article=Article {subject:file.source.subject.clone(),author:file.author.to_owned(),date:file.date.clone(),
                message_id:format!("<{}>",segment.message_id),bytes:i64::try_from(segment.bytes).unwrap_or(i64::MAX),
                filename:Some(file.filename.clone()),release_name:Some(batch.name.clone()),
                part:Some(if segment.number>0 {segment.number}else{self.segment as i64+1}),total_parts:Some(file.parts),
                file_index:Some(file.index),file_total:Some(file.total),..Default::default()};
            strings=strings.saturating_add(article_heap(&article));batch.articles.push(article);self.segment+=1;
        }
        Ok((!batch.articles.is_empty()).then_some(batch))
    }
}
fn release_heap(release:&Release)->usize {
    std::mem::size_of::<Release>()+[&release.name,&release.group,&release.poster,&release.date].into_iter()
        .map(String::capacity).sum::<usize>()+release.display_name.as_ref().map_or(0,String::capacity)
        +release.collection.as_ref().map_or(0,String::capacity)
}
fn article_heap(article:&Article)->usize {
    [&article.subject,&article.author,&article.date,&article.message_id,&article.references].into_iter()
        .map(String::capacity).sum::<usize>()+[&article.filename,&article.release_name].into_iter()
        .map(|value|value.as_ref().map_or(0,String::capacity)).sum::<usize>()
}
