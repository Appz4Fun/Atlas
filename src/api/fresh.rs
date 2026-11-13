//! Fresh native API dispatch. No historical release identity, shard opener or
//! provider fallback is reachable from this dispatch boundary.
use super::*;
use anyhow::{Context,Result,ensure};
use crate::pg::{self,fresh_reader::{ReadView,Budget}};
use std::time::Instant;
use std::path::PathBuf;

pub(super) fn unavailable()->ApiResponse {
    ApiResponse {status:503,failed:true,..ApiResponse::xml(503,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<error code=\"900\" description=\"Native catalog temporarily unavailable\"/>".into())}
}

pub(crate) fn spools()->Result<Arc<crate::nzb_spool::Pool>> {
    static POOL:Mutex<Option<(PathBuf,Arc<crate::nzb_spool::Pool>)>>=Mutex::new(None);
    let root=crate::paths::app_dir().join("nzb-spool");
    let mut slot=POOL.lock().unwrap_or_else(|error|error.into_inner());
    if let Some((bound,pool))=&*slot {ensure!(*bound==root,"native spool root changed");return Ok(pool.clone());}
    let mut directory=std::fs::DirBuilder::new();
    #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;directory.mode(0o700);}
    match directory.create(&root) {Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(error)=>return Err(error.into())}
    // Disk/concurrency admission, never a truncation of a stored name or body.
    // The shared pool accounts permits until sending/cancellation drops them.
    let pool=crate::nzb_spool::Pool::new(root.clone(),crate::nzb_spool::Limits {
        requests:16,per_request_bytes:16*1024*1024*1024,total_bytes:64*1024*1024*1024,free_bytes:128*1024*1024*1024})?;
    *slot=Some((root,pool.clone()));Ok(pool)
}

pub(crate) fn handle(storage:&pg::StorageConfig,args:&HashMap<String,String>,base:&str,key:&str)->ApiResponse {
    match args.get("t").map(String::as_str) {
        None=>return ApiResponse::error(200,"Missing parameter (t)"),
        Some("caps"|"get"|"files"|"search"|"tvsearch"|"movie"|"music"|"audio"|"book")=>{},
        Some(_)=>return ApiResponse::error(203,"Function not available"),
    }
    read(storage,|view|dispatch(view,args,base,key)).unwrap_or_else(|_|unavailable())
}

pub(crate) fn read<T>(storage:&pg::StorageConfig,work:impl FnOnce(&ReadView<'_>)->Result<T>)->Result<T> {
    read_with_budget(storage,Budget {rows:100,..Budget::default()},work)
}

pub(crate) fn read_with_budget<T>(storage:&pg::StorageConfig,budget:Budget,work:impl FnOnce(&ReadView<'_>)->Result<T>)->Result<T> {
    let until=Instant::now()+Duration::from_secs(10);
    let pool=pg::Pool::new(storage)?;
    let mut connection=pool.get_before(until)?;
    let view=ReadView::open(&mut connection,&storage.schema,budget,until)?;
    view.header_config(&crate::config::header_store_config())?;
    work(&view)
}

/// Shared by the real handler and isolated real-PG protocol fixtures. This
/// accepts a live readonly transaction, not a caller-provided empty/ready flag.
pub(crate) fn dispatch(view:&ReadView<'_>,args:&HashMap<String,String>,base:&str,key:&str)->Result<ApiResponse> {
    match args.get("t").map(String::as_str) {
        Some("caps")=>Ok(ApiResponse::xml(200,caps_body(base,&readable_ids()))),
        Some("files")=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            if !view.visible(id)? {return Ok(ApiResponse::error(300,"No such item"));}
            match view.file_list(id,&spools()?)? {
                Some(body)=>Ok(ApiResponse {spool:Some(body),..ApiResponse::xml(200,String::new())}),
                None=>Ok(ApiResponse::error(300,"No such item")),
            }
        }
        Some("get")=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            if !view.visible(id)? {return Ok(ApiResponse::not_found());}
            let pick=match (args.get("file"),args.get("name")) {
                (Some(ordinal),_)=>match ordinal.trim().parse::<u64>() {Ok(n) if n>0=>pg::fresh_nzb::Pick::Ordinal(n),_=>return Ok(ApiResponse::not_found())},
                (None,Some(name))=>pg::fresh_nzb::Pick::Name(name),
                _=>pg::fresh_nzb::Pick::Whole,
            };
            match view.nzb_own(id,pick,&spools()?)? {
                None=>Ok(ApiResponse::not_found()),
                Some(download)=>{
                    let mut response=ApiResponse {spool:Some(download.body),disposition:download.disposition,..ApiResponse::xml(200,String::new())};
                    response.content_type=Some("application/x-nzb");
                    if download.gaps>0 {response.headers.push(("X-Atlas-Incomplete".into(),download.gaps.to_string()));}
                    Ok(response)
                }
            }
        }
        Some(kind @("search"|"tvsearch"|"movie"|"music"|"audio"|"book"))=>search(view,kind,args,base,key),
        _=>Err(anyhow::anyhow!("native API operation changed")),
    }
}

/// These are supported read namespaces, independent of whether an external
/// provider is enabled. Retained IDs remain usable without provider traffic.
fn readable_ids()->crate::media::query::IdParams {
    use crate::media::query::{IdParams,IdSet};
    IdParams {tv:IdSet {tvdb:true,tvmaze:true,trakt:true,tmdb:true,imdb:true,rid:true},
        movie:IdSet {imdb:true,tmdb:true,trakt:true,..IdSet::default()}}
}
fn search(view:&ReadView<'_>,kind:&str,args:&HashMap<String,String>,base:&str,key:&str)->Result<ApiResponse> {
    let on=readable_ids();let ids=crate::media::query::from_args(kind,args,&on);
    let term=search_term(kind,args);let term=term.as_deref().unwrap_or("");
    if ids.is_none() && term.is_empty() && search_term(kind,args).is_none(){return Ok(rss_document(base,0,0,""));}
    let offset=args.get("offset").and_then(|value|value.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
    let limit=args.get("limit").and_then(|value|value.trim().parse::<usize>().ok()).unwrap_or(100).clamp(1,100);
    let filter=args.get("cat").and_then(|category|crate::category::parse_filter(category));let all=wants_incomplete(args);
    let mut rows=Vec::new();let mut at=offset;let mut total=None;let mut matched_ids=ids.as_ref();
    for batch in 0..if all{1}else{COMPLETE_SCAN_PAGES} {
        let mut page=view.api_query(term,matched_ids,filter.as_ref(),at,limit)?;
        // Original ID fallback: only when the ID matched nothing, never when
        // matching rows merely fail the incomplete filter.
        if batch==0 && page.rows.is_empty() && matched_ids.is_some() && !term.is_empty() {
            matched_ids=None;page=view.api_query(term,None,filter.as_ref(),at,limit)?;
        }
        let read=page.rows.len();total=page.total.or(total);at=at.checked_add(i64::try_from(read)?).context("native API page overflow")?;
        rows.extend(page.rows.into_iter().filter(|row|all||row.complete).take(limit-rows.len()));
        if read<limit||rows.len()>=limit{break;}
    }
    let ids:Vec<_>=rows.iter().map(|row|row.id).collect();let mut facts=view.api_facts(&ids)?;let mut items=String::new();
    for row in rows {
        let facts=facts.remove(&row.id).context("native API result facts disappeared")?;
        items.push_str(&rss_item(base,key,&row,&on,RssItemFacts {size:row.size.unwrap_or(0),complete:row.complete,repair:false,category:facts.category,
            password:facts.password.as_ref(),media:facts.media.as_ref(),copies:None,spot:None}));
    }
    let reached=offset.checked_add(i64::try_from(ids.len())?).context("native API total overflow")?;
    let total=total.map(i64::try_from).transpose()?.unwrap_or(reached).max(reached);
    Ok(rss_document(base,offset,total,&items))
}
