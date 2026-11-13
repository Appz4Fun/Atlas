//! Actual fresh-catalog GET dispatch. Static assets/login keep the common
//! outer path; signed-in reads never fall through to a legacy SQLite loader.
use super::*;
use anyhow::{Result,ensure,Context};
use crate::{pg::{self,fresh_reader::ReadView},word_index::native::{self,SearchSource}};

pub(super) fn get(path:&str,args:&HashMap<String,String>,base:&str,key:&str,storage:&pg::StorageConfig)->Option<ApiResponse> {
    if path=="/ui/settings" {return Some(html(200,crate::web_settings::settings_page(None,key)));}
    if path=="/ui/nzb" {
        let mut api_args=args.clone();api_args.insert("t".into(),"get".into());
        return Some(crate::api::fresh::handle(storage,&api_args,base,key));
    }
    let known=matches!(path,"/"|""|"/ui/results"|"/ui/total"|"/ui/release"|"/ui/duplicates"|"/ui/stats"|"/ui/details"|"/ui/pending"|
        "/ui/poster"|"/ui/cover"|"/ui/still"|"/ui/image"|"/ui/samepicture"|"/ui/picture"|"/ui/samefilm"|"/ui/same"|"/ui/meta"|"/ui/row"|
        "/ui/settings/groups/search"|"/ui/stats/live"|"/ui/stats/stored");
    if !known{return None;}
    Some(crate::api::fresh::read(storage,|view|dispatch(view,path,args,base,key)).unwrap_or_else(|_|{
        let body=diag("caution","native catalog unavailable","This view could not be read. Retry after PostgreSQL and its native readers recover.");
        let body=if matches!(path,"/"|""|"/ui/release"|"/ui/duplicates"|"/ui/stats"){page("Catalog unavailable · Atlas",&format!("{}<main>{body}</main>",masthead(true)))}else{body};
        let mut response=html(503,body);response.failed=true;response
    }))
}
fn dispatch(view:&ReadView<'_>,path:&str,args:&HashMap<String,String>,base:&str,_key:&str)->Result<ApiResponse> {
    match path {
        "/"|""|"/ui/results"|"/ui/total"=>{
            let mut search=Search::from_args(args);search.deep=false;
            let (rows,total)=query(view,&search)?;
            if path=="/ui/total" {return Ok(html(200,exact_html(usize::try_from(total)?)));}
            let ids:Vec<_>=rows.iter().map(|row|row.id).collect();let mut facts=view.listing_facts(&ids)?;
            let mut body=String::new();
            for row in &rows {
                let facts=facts.remove(&row.id).context("native result facts disappeared")?;
                body.push_str(&row_from_facts(row,ResultFacts {category:facts.category,password:facts.password.as_ref(),media:facts.media.as_ref(),
                    details:facts.details.as_ref().map(ResultDetails::from),poster:None,recovery:None,copies:None,spot:None,whole:None,repair:false}));
            }
            let rows_html=if body.is_empty(){"<p class=\"note dim\">No releases match.</p>".to_owned()}else{table(&search,&body)};
            let content=format!("{}<p class=\"results-summary\">{} releases</p>{rows_html}",search.filter_links(),thousands(i64::try_from(total)?));
            if matches!(path,"/"|""){return Ok(html(200,search_page_from_results(&search,base,&content,true)));}
            if search.offset>0 {return Ok(html(200,body));}
            let content=format!("{content}{}{}{}{}",category_nav(&search," hx-swap-oob=\"true\""),category_input(&search.cat," hx-swap-oob=\"true\""),scope_inputs_for(&search," hx-swap-oob=\"true\"",false),ask_note(""," hx-swap-oob=\"true\""));
            let mut response=html(200,content);response.headers.push(("HX-Push-Url".into(),format!("/?{}",search.query(&search.cat,0))));Ok(response)
        }
        "/ui/details"=>{
            let id=args.get("id").and_then(|id|id.parse().ok()).unwrap_or(0);
            if view.release(id)?.is_none(){return Ok(html(404,String::new()));}
            let facts=view.listing_facts(&[id])?.remove(&id).context("native details facts disappeared")?;
            // Actual cache absence is a known empty lazy detail panel. No
            // mutable provider fallback or SQLite cache warmer is invoked.
            Ok(html(200,facts.details.as_ref().map(|details|details_html(details,None)).unwrap_or_default()))
        }
        _=>anyhow::bail!("fresh native view loader is not yet connected"),
    }
}
fn query(view:&ReadView<'_>,search:&Search)->Result<(Vec<ReleaseRow>,u64)> {
    if view.count()?==0{return Ok((Vec::new(),0));}
    ensure!(!search.fields.any() && search.min_score.is_none() && !search.hide_incomplete && !search.hides_spots() && !search.filters.contains(&HIDE_COPIES),
        "fresh native advanced filtering is not yet connected");
    if search.q.is_empty() && search.filters.is_empty() && search.cat.is_empty() && search.sort.is_none() {
        return Ok((view.recent(search.offset,PAGE_SIZE as usize)?,view.visible_count()?));
    }
    let mut source=pg::fresh_words::Search::open(view)?;let stamp=source.stamp()?;
    let directory=view.word_index_root(&crate::config::header_store_config())?;
    let index=word_index::WordIndex::read_native(&directory,stamp)?;
    let by=match search.sort {Some(SortBy::Name)=>word_index::By::Name,Some(SortBy::Category)=>word_index::By::Category,
        Some(SortBy::Size)=>word_index::By::Size,Some(SortBy::Posted)=>word_index::By::Posted,Some(SortBy::Group)=>word_index::By::Group,
        Some(SortBy::Rating)=>anyhow::bail!("fresh native rating order is not connected"),None if search.q.is_empty()=>word_index::By::Newest,None=>word_index::By::Relevance};
    let found=index.search_native(&mut source,&search.q,parse_filter(&search.cat).as_ref(),&search.tag_filters(),by,search.descending,
        usize::try_from(search.offset)?,PAGE_SIZE as usize,native::Budget {rows:100,..native::Budget::default()})?;
    Ok((found.rows,u64::try_from(found.total)?))
}
