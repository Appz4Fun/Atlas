//! Actual fresh-catalog GET dispatch. Static assets/login keep the common
//! outer path; signed-in reads never fall through to a legacy SQLite loader.
use super::*;
use anyhow::{Result,ensure,Context};
use crate::{pg::{self,fresh_reader::ReadView},word_index::native::SearchSource};

pub(super) fn get(path:&str,args:&HashMap<String,String>,base:&str,key:&str,storage:&pg::StorageConfig)->Option<ApiResponse> {
    get_with_assets(path,args,base,key,storage,None)
}
pub(super) fn get_with_assets(path:&str,args:&HashMap<String,String>,base:&str,key:&str,storage:&pg::StorageConfig,assets:Option<&pg::fresh_assets::AssetManifest>)->Option<ApiResponse> {
    if path=="/ui/meta" && !args.get("part").is_some_and(|part|matches!(part.as_str(),"headers"|"parts"|"hashes")){return Some(html(400,String::new()));}
    if path=="/ui/settings" {return Some(html(200,crate::web_settings::settings_page(None,key)));}
    if path=="/ui/nzb" {
        if args.get("fill").is_some_and(|fill|fill=="1") {
            let explanation=diag("caution","Verified gap filling is unavailable",
                "This native catalog has no verified donor payloads for gap filling. Retry without fill=1 to request the release's own NZB.");
            let mut response=html(503,page("Gap filling unavailable · Atlas",&format!("{}<main>{explanation}</main>",masthead(true))));
            response.failed=true;return Some(response);
        }
        let mut api_args=args.clone();api_args.insert("t".into(),"get".into());
        return Some(crate::api::fresh::handle(storage,&api_args,base,key));
    }
    let known=matches!(path,"/"|""|"/ui/results"|"/ui/total"|"/ui/release"|"/ui/duplicates"|"/ui/stats"|"/ui/details"|"/ui/pending"|
        "/ui/poster"|"/ui/cover"|"/ui/still"|"/ui/image"|"/ui/samepicture"|"/ui/picture"|"/ui/samefilm"|"/ui/same"|"/ui/meta"|"/ui/row"|
        "/ui/settings/groups/search"|"/ui/stats/live"|"/ui/stats/stored");
    if !known{return None;}
    // Statistics retain current labels plus bounded two-frame/reduction
    // workspace; the worker count still bounds concurrent requests. Actual
    // runtime resource qualification is required before native activation.
    let budget=pg::fresh_reader::Budget {rows:100,bytes:if matches!(path,"/ui/stats"|"/ui/stats/live"){64*1024*1024}else{4*1024*1024}};
    Some(crate::api::fresh::read_with_budget(storage,budget,|view|dispatch_with_assets(view,path,args,base,key,assets)).unwrap_or_else(|_|{
        let body=diag("caution","native catalog unavailable","This view could not be read. Retry after PostgreSQL and its native readers recover.");
        let body=if matches!(path,"/"|""|"/ui/release"|"/ui/duplicates"|"/ui/stats"){page("Catalog unavailable · Atlas",&format!("{}<main>{body}</main>",masthead(true)))}else{body};
        let mut response=html(503,body);response.failed=true;response
    }))
}
fn dispatch(view:&ReadView<'_>,path:&str,args:&HashMap<String,String>,base:&str,_key:&str)->Result<ApiResponse> {
    dispatch_with_assets(view,path,args,base,_key,None)
}
fn dispatch_with_assets(view:&ReadView<'_>,path:&str,args:&HashMap<String,String>,base:&str,_key:&str,assets:Option<&pg::fresh_assets::AssetManifest>)->Result<ApiResponse> {
    if matches!(path,"/ui/release"|"/ui/same"|"/ui/details"|"/ui/pending"|"/ui/cover"|"/ui/still"|"/ui/image"|"/ui/samepicture"|"/ui/picture"|"/ui/samefilm"|"/ui/meta"|"/ui/row") {
        let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
        if !view.visible(id)? {
            return Ok(if matches!(path,"/ui/release"|"/ui/same"){release_problem(404,"note","release not found","No such release is in the fresh catalog.")}else{html(404,String::new())});
        }
    }
    match path {
        "/"|""|"/ui/results"|"/ui/total"=>{
            let mut search=Search::from_args(args);search.deep=false;
            let (rows,total)=query(view,&search)?;
            if path=="/ui/total" {return Ok(html(200,if total.exact{exact_html(usize::try_from(total.count)?)}else{format!("<strong class=\"total\">{}</strong>",escape(&total.describe()))}));}
            let ids:Vec<_>=rows.iter().map(|row|row.id).collect();let mut facts=view.listing_facts(&ids)?;
            let mut body=String::new();
            for row in &rows {
                let facts=facts.remove(&row.id).context("native result facts disappeared")?;
                body.push_str(&row_from_facts(row,ResultFacts {category:facts.category,password:facts.password.as_ref(),media:facts.media.as_ref(),
                    details:facts.details.as_ref().map(ResultDetails::from),poster:None,recovery:None,copies:None,spot:None,whole:None,repair:false}));
            }
            let next=search.offset.checked_add(PAGE_SIZE).context("native result page offset overflow")?;
            if next<total.count {body.push_str(&search.load_more());}
            if !total.exact && next>=total.count {
                body.push_str(&format!("<tr><td colspan=\"{COLUMNS}\" class=\"note dim\">Results beyond this search window have not been counted. Narrow the search to see further.</td></tr>"));
            }
            let rows_html=if body.is_empty(){"<p class=\"note dim\">No releases match.</p>".to_owned()}else{table(&search,&body)};
            let content=format!("{}<p class=\"results-summary\">{}</p>{rows_html}",search.filter_links(),escape(&total.describe()));
            if matches!(path,"/"|""){return Ok(html(200,search_page_from_results(&search,base,&content,true)));}
            if search.offset>0 {return Ok(html(200,body));}
            let content=format!("{content}{}{}{}{}",category_nav(&search," hx-swap-oob=\"true\""),category_input(&search.cat," hx-swap-oob=\"true\""),scope_inputs_for(&search," hx-swap-oob=\"true\"",false),ask_note(""," hx-swap-oob=\"true\""));
            let mut response=html(200,content);response.headers.push(("HX-Push-Url".into(),format!("/?{}",search.query(&search.cat,0))));Ok(response)
        }
        "/ui/meta" if args.get("part").is_some_and(|part|part=="headers")=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            Ok(html(200,view.metadata_headers(id)?.context("native header release disappeared")?))
        }
        "/ui/meta" if args.get("part").is_some_and(|part|part=="parts")=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            let facts=view.metadata_parts(id)?.context("native parts release disappeared")?;
            Ok(html(200,if facts.files==0 {"<p class=\"dim\">This release has no files on record.</p>".into()}else{crate::web_meta::parts_from_facts(&facts)}))
        }
        "/ui/meta" if args.get("part").is_some_and(|part|part=="hashes")=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            let (summary,probe)=view.metadata_hashes(id,&crate::api::fresh::spools()?)?.context("native hash release disappeared")?;
            Ok(html(200,crate::web_meta::hashes_from_facts(probe.as_ref(),&summary)))
        }
        "/ui/pending"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            let current=view.processing(id)?.context("native processing release disappeared")?;
            let pace=view.processing_pace(current.owner.as_ref(),current.now)?;
            let waits=view.processing_waits(&current.state,&pace,current.now)?;
            let cfg=crate::config::load_config().context("native processing configuration unavailable")?;
            let rules=view.processing_rules(&current.state,current.now,&cfg)?;
            let body=crate::web_pending::native_with_pace(&current.state,&pace,&waits,&rules,current.now)?;
            view.verify_runtime(current.owner.as_ref())?;Ok(html(200,body))
        }
        "/ui/picture"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);let name=args.get("name").map(String::as_str).unwrap_or_default();
            if !crate::images::is_image_name(name){return Ok(html(400,r#"<div class="picture-state">Not a picture.</div>"#.into()));}
            if !view.has_own_picture(id,name)? {return Ok(html(404,r#"<div class="picture-state">No such picture in this release.</div>"#.into()));}
            let pictures=view.own_pictures(id)?;let etas=native_picture_etas(view,&pictures,[name])?;let state=native_picture_state(name,&pictures,&etas);
            if state==PictureState::Shown {return Ok(html(200,format!(r#"<div class="picture-state">{} <a href="/ui/release?id={id}#pictures">reload to see it</a></div>"#,tag("tag-ok","SHOWN"))));}
            Ok(html(200,native_picture_state_html(id,name,&state,"")))
        }
        "/ui/poster"|"/ui/cover"|"/ui/still"|"/ui/image"=>{
            asset_fragment(view,path,args,assets,&crate::api::fresh::spools()?)
        }
        "/ui/settings/groups/search"=>{
            let query=args.get("q").map_or("",String::as_str).trim();
            if query.chars().count()<3 {return Ok(html(200,crate::web_settings::group_results(Err("Search at least 3 characters.".into()),_key)));}
            let cfg=crate::config::load_config().context("native group configuration unavailable")?;
            if cfg.servers.is_empty(){return Ok(html(200,crate::web_settings::group_results(Err("No Usenet server is configured.".into()),_key)));}
            let Some(capture)=view.captured_groups(&cfg.servers,query)? else{
                let mut response=html(503,diag("caution","Provider inventory unavailable","No completed LIST capture covers this query for a configured provider account."));
                response.failed=true;return Ok(response);
            };
            let captured=chrono::DateTime::from_timestamp(capture.captured_at,0).context("provider inventory capture date is invalid")?;
            let body=format!("<p class=\"hint\">Provider inventory captured {}.</p>{}",captured.format("%Y-%m-%d %H:%M:%S UTC"),
                crate::web_settings::group_results(Ok(capture.groups),_key));
            Ok(html(200,body))
        }
        "/ui/stats"|"/ui/stats/live"=>{
            let facts=view.live_statistics()?;
            Ok(html(200,if path=="/ui/stats"{crate::web_stats::native_page_from_facts(&facts)}else{crate::web_stats::native_live_from_facts(&facts)}))
        }
        "/ui/stats/stored"=>{
            let now=view.statistics_clock()?;let stored=view.stored_statistics(now)?;
            Ok(html(200,crate::web_stats::stored_html(&stored,now as f64)))
        }
        "/ui/samepicture"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            Ok(html(200,same_picture_html(&view.same_pictures(id)?)))
        }
        "/ui/samefilm"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            Ok(html(200,same_film_html(&view.same_film(id)?)))
        }
        "/ui/same"=>{
            let Some(kind)=args.get("kind").and_then(|kind|crate::keys::Kind::parse(kind)) else {
                return Ok(release_problem(400,"caution","no such key","The link names a key Atlas doesn't keep."));
            };
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            let found=view.same_key(id,kind)?.context("native related release disappeared")?;
            Ok(same_key_page_from_facts(&found.release,kind,found.has,&found.rows))
        }
        "/ui/release"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            Ok(html(200,own_release_page(view,id,_key)?))
        }
        "/ui/row"=>{
            let id=args.get("id").and_then(|id|id.trim().parse().ok()).unwrap_or(0);
            let as_card=args.get("as").is_some_and(|value|value=="card");
            Ok(html(200,view.own_drawer(id,as_card)?.context("native drawer release disappeared")?))
        }
        "/ui/details"=>{
            let id=args.get("id").and_then(|id|id.parse().ok()).unwrap_or(0);
            if view.release(id)?.is_none(){return Ok(html(404,String::new()));}
            let facts=view.listing_facts(&[id])?.remove(&id).context("native details facts disappeared")?;
            // Actual cache absence is a known empty lazy detail panel. No
            // mutable provider fallback or SQLite cache warmer is invoked.
            let seconds=view.probe(id)?.and_then(|probe|probe.media.and_then(|media|media.duration_secs));
            Ok(html(200,facts.details.as_ref().map(|details|details_html(details,seconds)).unwrap_or_default()))
        }
        _=>anyhow::bail!("fresh native view loader is not yet connected"),
    }
}
fn asset_fragment(view:&ReadView<'_>,path:&str,args:&HashMap<String,String>,assets:Option<&pg::fresh_assets::AssetManifest>,pool:&Arc<crate::nzb_spool::Pool>)->Result<ApiResponse> {
    let Some(request)=view.asset_request(path,args)? else{return Ok(ApiResponse::not_found())};
    let assets=assets.context("retained asset manifest/root has not been admitted")?;
    let Some(body)=view.read_asset(assets,&request,pool)? else{return Ok(ApiResponse::not_found())};
    let mut response=ApiResponse::xml(200,String::new());response.content_type=Some(request.content_type);response.spool=Some(body);
    response.headers=vec![("Cache-Control".into(),"private, max-age=86400".into()),("X-Content-Type-Options".into(),"nosniff".into()),
        ("Content-Security-Policy".into(),"default-src 'none'; img-src 'self'; sandbox".into())];Ok(response)
}

fn native_picture_etas<'name>(view:&ReadView<'_>,pictures:&pg::fresh_pictures::Pictures,names:impl IntoIterator<Item=&'name str>)->Result<HashMap<i64,String>> {
    let names:HashSet<_>=names.into_iter().collect();let mut effective:HashMap<&str,&crate::images::step::Request>=HashMap::new();
    for request in pictures.requests.iter().filter(|request|names.contains(request.file.as_str())) {
        // The request reader is ordered after the own row, as in the legacy
        // request list; equal timestamps retain the same last-row precedence.
        effective.entry(request.file.as_str()).and_modify(|old|{if request.requested_at>=old.requested_at{*old=request;}}).or_insert(request);
    }
    let mut etas=HashMap::new();
    for request in effective.into_values().filter(|request|request.done_at.is_none()&&!request.passed_over
        &&!pictures.images.iter().any(|(_,image)|image.file==request.file)) {
        if let std::collections::hash_map::Entry::Vacant(entry)=etas.entry(request.release_id) {
            entry.insert(view.picture_eta(request.release_id)?.unwrap_or_else(||"native processing status is unavailable".into()));
        }
    }Ok(etas)
}
fn native_picture_state(name:&str,pictures:&pg::fresh_pictures::Pictures,etas:&HashMap<i64,String>)->PictureState {
    if pictures.images.iter().any(|(_,image)|image.file==name){return PictureState::Shown;}
    match pictures.requests.iter().filter(|request|request.file==name).max_by_key(|request|request.requested_at) {
        Some(request) if request.done_at.is_none()&&request.passed_over=>PictureState::Failed("the pictures step isn't reading this release".into()),
        Some(request) if request.done_at.is_none()=>PictureState::Queued(etas.get(&request.release_id).cloned().unwrap_or_else(||"native processing status is unavailable".into())),
        Some(request) if request.outcome.as_deref().is_some_and(|outcome|outcome!=crate::images::step::Answer::Shown.word())=>PictureState::Failed(request.outcome.clone().unwrap_or_default()),
        _=>PictureState::Offer,
    }
}
fn native_picture_state_html(id:i64,name:&str,state:&PictureState,csrf:&str)->String {
    let unavailable="Picture requests are unavailable while native request handling is being qualified.";
    match state {
        PictureState::Shown|PictureState::Queued(_)=>picture_state_html(id,name,state,csrf),
        PictureState::Failed(why)=>format!(r#"<div class="picture-state">{} <span class="dim">{}</span> <span class="dim">{unavailable}</span></div>"#,tag("tag-bad","NOT SHOWN"),escape(why)),
        PictureState::Offer=>format!(r#"<div class="picture-state dim">{unavailable}</div>"#),
    }
}

fn own_release_page(view:&ReadView<'_>,id:i64,key:&str)->Result<String> {
    let row=view.release(id)?.context("native release disappeared")?;
    let names=view.own_names(id)?.context("native release name disappeared")?;
    let inner=view.own_inner(id)?;let contents=view.own_contents(id)?;
    let listing=view.listing_facts(&[id])?.remove(&id).context("native release display facts disappeared")?;
    let groups=[release_info::GroupPosting {id,group:row.group_name.clone(),articles:row.parts.unwrap_or(0)}];
    // An own row's completion is the native writer's verdict. Unqualified
    // cross-row/PAR2 relationships never turn it into repaired/lent payload.
    let (shown,raw,tags,facts)=release_header_from_facts(&row,&inner,listing.media.as_ref(),Some(&names),&contents,None,
        ReleaseHeaderFacts {category:listing.category,details:listing.details.as_ref(),spot:None,spot_removed:false,repair:false,groups:&groups,password:""});
    let found=view.probe(id)?.map(|probe|seven_z_first_file(probe,&inner,&contents));
    let nfo=match found.as_ref().and_then(|probe|probe.nfo.as_ref().map(|text|(text,&probe.imdb_ids))) {
        Some((text,ids))=>nfo_html(text,ids),
        None=>view.own_nfo_text(id)?.map(|text|nfo_html(&text,&[])).unwrap_or_default(),
    };
    let subtitles=view.own_subtitles(id)?;let salt=view.own_key_salt()?;
    let probe=found.as_ref().map(|probe|probe_html(probe,subtitles.as_ref(),None,salt)).unwrap_or_default();
    // Full-page relationship/asset qualification is deliberately separate
    // from these real own facts. Unavailability is visible, never absence.
    let warning=diag("note","Additional release panels unavailable",
        "Related postings and sidecar lookups, password hints and borrowed artwork are unavailable in the native reader. Own picture bytes require the selected asset root and manifest.");
    let inputs=crate::web_meta::MetaInputs {release:&row,names:Some(&names),inner:&inner,contents:&contents,probe:found.as_ref()};
    let metadata=view.own_metadata(&inputs)?;
    let meta=crate::web_meta::meta_from_facts(&inputs,&metadata);
    let pictures=view.own_pictures(id)?;view.admit_picture_cells(&contents)?;let etas=native_picture_etas(view,&pictures,contents.list.iter().filter(|file|crate::images::is_image_name(&file.name)).map(|file|file.name.as_str()))?;
    let mut states=HashMap::new();
    for file in contents.list.iter().filter(|file|crate::images::is_image_name(&file.name)) {
        states.insert(file.name.clone(),native_picture_state(&file.name,&pictures,&etas));
    }
    let nzb_files=nzb_files_html_with(id,&contents,&states,&crate::web_settings::csrf_token(key),native_picture_state_html);
    let files=inner_files_html(&inner,false);
    Ok(release_page_from_facts(&row,&shown,raw.as_deref(),&tags,&facts,listing.category,
        ReleaseSections {fill:String::new(),warning,spot:String::new(),nfo,images:images_html(id,&pictures.images),stills:stills_html(id,&pictures.stills,None),copies:String::new(),meta,probe,nzb_files,files}))
}

fn query(view:&ReadView<'_>,search:&Search)->Result<(Vec<ReleaseRow>,crate::field_search::Total)> {
    query_using_headers(view,search,&crate::config::header_store_config())
}
fn query_using_headers(view:&ReadView<'_>,search:&Search,headers:&crate::config::HeaderStoreConfig)->Result<(Vec<ReleaseRow>,crate::field_search::Total)> {
    let counted=|rows,total|->Result<_>{Ok((rows,crate::field_search::Total {count:i64::try_from(total)?,exact:true}))};
    if view.count()?==0{return counted(Vec::new(),0u64);}
    ensure!(!search.filters.contains(&HIDE_COPIES),"native copy filtering requires verified relationship facts");
    let tags=search.tag_filters();let categories=parse_filter(&search.cat);
    let fielded=search.fields.any()&&search.fields!=(Fields {group:search.fields.group.clone(),..Fields::default()});
    if fielded || (search.q.trim().is_empty()&&tags.is_empty()) {
        let (rows,total)=view.field_search_with(&search.fields,&search.q,categories.as_ref(),search.offset,PAGE_SIZE as usize,
            &tags,search.sort,search.descending,search.hide_incomplete,search.hides_spots(),search.min_score)?;
        return counted(rows,total);
    }
    let mut source=pg::fresh_words::Search::open(view)?;let stamp=source.stamp()?;
    let directory=view.word_index_root(headers)?;
    let index=word_index::WordIndex::read_native(&directory,stamp)?;
    if let Some(sort @ (SortBy::Name|SortBy::Group|SortBy::Posted))=search.sort {
        let group=(!search.fields.group.is_empty()).then_some(search.fields.group.as_str());
        let (rows,total)=view.sorted_word_search(&index,&mut source,&search.q,categories.as_ref(),&tags,group,search.offset,PAGE_SIZE as usize,
            sort,search.descending,search.hide_incomplete,search.hides_spots(),search.min_score)?;
        return counted(rows,total);
    }
    let rating=search.sort==Some(SortBy::Rating);
    let by=match search.sort {Some(SortBy::Name)=>word_index::By::Name,Some(SortBy::Category)=>word_index::By::Category,
        Some(SortBy::Size)=>word_index::By::Size,Some(SortBy::Posted)=>word_index::By::Posted,Some(SortBy::Group)=>word_index::By::Group,
        Some(SortBy::Rating)|None if search.q.is_empty()=>word_index::By::Newest,Some(SortBy::Rating)|None=>word_index::By::Relevance};
    let group=(!search.fields.group.is_empty()).then_some(search.fields.group.as_str());
    if search.min_score.is_none()&&!search.hide_incomplete&&!search.hides_spots()&&!rating {
        let offset=usize::try_from(search.offset)?;let budget=view.word_page_budget(&index,offset,PAGE_SIZE as usize)?;
        let found=index.search_native_in_group(&mut source,&search.q,categories.as_ref(),&tags,group,by,search.descending,
            offset,PAGE_SIZE as usize,budget)?;
        return counted(found.rows,u64::try_from(found.total)?);
    }
    // Preserve the existing bounded rating/ranked windows. Candidate IDs and
    // PG argument/selection workspace are admitted before Tantivy allocates;
    // unexamined matches never become a fabricated exact filtered total.
    let maximum=if search.min_score.is_some(){GROW_WITH_RATING}else if rating{SORT_WINDOW.max(search.offset.checked_add(PAGE_SIZE+1).context("native rating offset overflow")?).min(MAX_WINDOW)}else{MAX_WINDOW};
    let (window,budget)=view.ranked_search_window(&index,usize::try_from(maximum)?)?;
    let (ids,all)=index.ranked_native_ids_in_group(&mut source,&search.q,categories.as_ref(),&tags,group,by,search.descending,0,window,budget)?;
    let exact=ids.len()>=all;
    let (rows,total)=view.filter_ranked_search(&ids,search.offset,PAGE_SIZE as usize,rating,search.descending,search.hide_incomplete,search.hides_spots(),search.min_score)?;
    Ok((rows,crate::field_search::Total {count:i64::try_from(total)?,exact}))
}

#[cfg(test)]
mod fill_request_tests {
    use super::*;

    #[test]
    fn native_fill_request_is_explicitly_unavailable_before_catalog_access() {
        // An invalid schema is rejected before any connection can be opened.
        // The fill response must explain the unavailable operation even then,
        // rather than delegate to the own-payload download or its database error.
        let storage=pg::StorageConfig {schema:"invalid schema".into(),..Default::default()};
        for selection in [None,Some(("file","1")),Some(("name","sample.bin"))] {
            let mut args=HashMap::from([("id".into(),"42".into()),("fill".into(),"1".into())]);
            if let Some((key,value))=selection {args.insert(key.into(),value.into());}
            let response=get("/ui/nzb",&args,"http://example.invalid","fixture-key",&storage).unwrap();
            assert_eq!(response.status,503);assert!(response.failed);
            assert_eq!(response.content_type,Some("text/html; charset=utf-8"));
            assert!(response.spool.is_none()&&response.bytes.is_none()&&response.disposition.is_none());
            assert!(response.body.contains("Verified gap filling is unavailable"),"{}",response.body);
            assert!(response.body.contains("verified donor payloads")&&response.body.contains("without fill=1"));
            assert!(response.body.contains("<footer class=\"site-footer\" role=\"contentinfo\">"));
            assert!(response.body.contains("https://www.patreon.com/cw/xbmc4lyfe"));
            assert!(!response.body.contains("<nzb")&&!response.body.contains("fixture-key"));
        }
    }

    #[test]
    fn own_download_selection_is_not_replaced_by_the_fill_refusal() {
        let storage=pg::StorageConfig {schema:"invalid schema".into(),..Default::default()};
        for fill in [None,Some("0"),Some("yes")] {
            let mut args=HashMap::from([("id".into(),"42".into())]);
            if let Some(fill)=fill {args.insert("fill".into(),fill.into());}
            let response=get("/ui/nzb",&args,"http://example.invalid","fixture-key",&storage).unwrap();
            assert_eq!(response.status,503);assert!(response.failed);
            assert_eq!(response.content_type,Some("application/xml; charset=utf-8"));
            assert!(response.body.contains("Native catalog temporarily unavailable"));
            assert!(!response.body.contains("Verified gap filling is unavailable"));
        }
    }

    #[test]
    fn signed_in_native_get_reaches_fill_refusal_and_unsigned_get_keeps_authentication() {
        let storage=pg::StorageConfig {backend:pg::Backend::Postgres,schema:"invalid schema".into(),..Default::default()};
        let cookies=format!("{COOKIE}={}",session_token("fixture-key"));
        let response=super::super::get_with_storage("/ui/nzb","id=42&fill=1",Some(&cookies),"http://example.invalid","fixture-key",&storage).unwrap();
        assert_eq!(response.status,503);assert!(response.failed);
        assert!(response.body.contains("Verified gap filling is unavailable"));
        let unsigned=super::super::get_with_storage("/ui/nzb","id=42&fill=1",None,"http://example.invalid","fixture-key",&storage).unwrap();
        assert_eq!(unsigned.status,303);assert!(!unsigned.failed);
        assert!(unsigned.headers.iter().any(|(name,value)|name=="Location"&&value=="/"));
        assert!(unsigned.body.is_empty()&&unsigned.spool.is_none());
    }
}

#[cfg(test)]
mod drawer_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated fresh schema9 and native handler read admission"]
    fn actual_native_drawer_handler_renders_row_card_and_missing_release() {
        let schema=pg::testing::Schema::new().expect("explicit fresh schema9");let mut client=schema.pool.get().unwrap();
        pg::release_writer::save(&mut client,&[Release {name:"Drawer.Example.Release".into(),poster:"example & poster".into(),group:"alt.binaries.test".into(),date:"2020-01-01".into(),
            articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:"<drawer@example.test>".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        client.execute("insert into release_nfo(release_id,file,text) values($1,'note.nfo','<script>&quoted')",&[&id]).unwrap();
        client.execute("insert into release_opensubtitles(release_id,moviehash,checked_at) values($1,'0123456789abcdef',1)",&[&id]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let mut args=HashMap::from([("id".into(),id.to_string())]);
        let row=dispatch(&view,"/ui/row",&args,"http://example.test","fixture-key").unwrap();
        assert_eq!(row.status,200);assert!(row.body.starts_with("<tr class=\"drawer-row\">"),"{}",row.body);
        assert!(row.body.contains(&format!("/ui/release?id={id}")),"{}",row.body);
        assert!(row.body.contains("Contents")&&row.body.contains("From shape"),"{}",row.body);
        args.insert("as".into(),"card".into());
        let card=dispatch(&view,"/ui/row",&args,"http://example.test","fixture-key").unwrap();
        assert_eq!(card.status,200);assert!(card.body.starts_with("<div class=\"drawer-card\">"),"{}",card.body);
        let subtitles=view.own_subtitles(id).unwrap().unwrap();assert_eq!(subtitles.moviehash,"0123456789abcdef");assert!(subtitles.feature.is_none(),"negative kept answer remains present");
        let release=dispatch(&view,"/ui/release",&args,"http://example.test","fixture-key").unwrap();
        assert_eq!(release.status,200);assert!(release.body.contains("<footer class=\"site-footer\" role=\"contentinfo\">"),"{}",release.body);
        assert!(release.body.contains("https://github.com/Eraxty")&&release.body.contains("https://github.com/xbmc4lyfe")&&release.body.contains("https://www.patreon.com/cw/xbmc4lyfe"));
        assert!(release.body.contains("Files in the NZB")&&release.body.contains("video.mkv"),"{}",release.body);
        assert!(release.body.contains("&lt;script&gt;&amp;quoted")&&!release.body.contains("<script>&quoted"),"{}",release.body);
        assert!(release.body.contains("Additional release panels unavailable"),"unported panels must not appear as known absence");
        assert!(!release.body.contains("gaps filled from a twin"));
        args.insert("id".into(),"0".into());assert_eq!(dispatch(&view,"/ui/row",&args,"http://example.test","fixture-key").unwrap().status,404);
    }
}

#[cfg(test)]
mod field_reader_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires explicit isolated fresh schema9 and native handler read admission"]
    fn native_field_search_preserves_literal_predicates_and_never_needs_a_word_index() {
        let schema=pg::testing::Schema::new().expect("explicit fresh PostgreSQL schema9");
        let mut client=schema.pool.get().unwrap();
        let releases=[("Alpha.Movie.2020-GRP","Poster <first%_@example.invalid>",64),
            ("Beta.Movie.2021-OTHER","Other <second@example.invalid>",128)].into_iter().enumerate().map(|(n,(name,poster,bytes))|Release {
            name:name.into(),poster:poster.into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some("video.mkv".into()),subject:"\"video.mkv\" yEnc (1/1)".into(),
                message_id:format!("field-{n}@example.invalid"),part:Some(1),total_parts:Some(1),bytes,..Default::default()}],..Default::default()
        }).collect::<Vec<_>>();
        let saved=pg::release_writer::save(&mut client,&releases,1).unwrap();assert_eq!(saved.releases,2);
        let id:i64=client.query_one("select id from releases where name='Alpha.Movie.2020-GRP'",&[]).unwrap().get(0);
        client.execute("insert into release_probe(release_id,crc32,agent,info) values($1,2864434397,'Tool 1','{}')",&[&id]).unwrap();
        for fields in [vec![("poster","Poster <first%_@example.invalid>")],vec![("email","FIRST%_@EXAMPLE.INVALID")],
            vec![("size","64")],vec![("team","grp")],vec![("crc","aabbccdd")],vec![("agent","Tool 1")],
            vec![("q","ALPHA"),("poster","Poster <first%_@example.invalid>"),("group","alt.binaries.test"),("size","64"),("crc","0xaabbccdd")]] {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let args=fields.into_iter().map(|(k,v)|(k.into(),v.into())).collect();
            let response=dispatch(&view,"/ui/results",&args,"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("Alpha.Movie.2020-GRP"),"{}",response.body);
            assert!(!response.body.contains("Beta.Movie.2021-OTHER"),"{}",response.body);
        }
        for fields in [vec![("poster","poster <first%_@example.invalid>")],vec![("email","first__@example.invalid")],
            vec![("size","invalid")],vec![("crc","not-hex")],vec![("agent","tool 1")]] {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let args=fields.into_iter().map(|(k,v)|(k.into(),v.into())).collect();
            let response=dispatch(&view,"/ui/results",&args,"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("No releases match."),"{}",response.body);
        }
    }
}

#[cfg(test)]
mod relationship_reader_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema9"]
    fn same_key_handler_reads_real_candidates_without_lending_hiding_or_exposing_keys() {
        let schema=pg::testing::Schema::new().expect("explicit fresh PostgreSQL schema9");let mut client=schema.pool.get().unwrap();
        let releases=["Own.Movie.Release","Older.Linked.Release","Unrelated.Release","Newest.Linked.Release"].into_iter().enumerate().map(|(n,name)|Release {
            name:name.into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:format!("related-{n}@example.invalid"),
                part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()
        }).collect::<Vec<_>>();
        assert_eq!(pg::release_writer::save(&mut client,&releases,1).unwrap().releases,4);
        let ids=client.query("select id from releases order by id",&[]).unwrap().into_iter().map(|row|row.get::<_,i64>(0)).collect::<Vec<_>>();
        for id in [ids[0],ids[1],ids[3]] {client.execute("insert into release_keys(release_id,layout) values($1,-6378619621457769803)",&[&id]).unwrap();}
        let view=ReadView::open(&mut client,&schema.name,Budget {rows:100,..Budget::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        let mut args=HashMap::from([("id".into(),ids[0].to_string()),("kind".into(),"layout".into())]);
        let found=dispatch(&view,"/ui/same",&args,"http://example.invalid","fixture-key").unwrap();assert_eq!(found.status,200);
        assert!(found.body.find("Newest.Linked.Release").unwrap()<found.body.find("Older.Linked.Release").unwrap());
        assert!(!found.body.contains("Unrelated.Release"));assert!(!found.body.contains("-6378619621457769803"));
        assert!(found.body.contains("site-footer")&&found.body.contains("https://www.patreon.com/cw/xbmc4lyfe"));
        assert!(!found.body.contains("gaps filled")&&!found.body.contains("TWIN"));
        args.insert("kind".into(),"host".into());let missing=dispatch(&view,"/ui/same",&args,"http://example.invalid","fixture-key").unwrap();
        assert_eq!(missing.status,200);assert!(missing.body.contains("this release has no such key"));
        args.insert("kind".into(),"injected-column".into());assert_eq!(dispatch(&view,"/ui/same",&args,"http://example.invalid","fixture-key").unwrap().status,400);
    }
}

#[cfg(test)]
mod stored_stats_reader_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::fresh_reader::Budget};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires isolated fresh PostgreSQL with the owner-approved posted_clock writer/schema"]
    fn stored_stats_handler_counts_real_posting_times_and_keeps_missing_timestamp_unavailable() {
        let schema=pg::testing::Schema::new().expect("explicit fresh PostgreSQL schema");let mut client=schema.pool.get().unwrap();
        let now=chrono::Utc::now();
        let releases=[("Current.Complete.Release",now.to_rfc2822(),1,17),
            ("Current.Partial.Release",now.to_rfc2822(),3,23),("Old.Release",(now-chrono::Duration::days(2)).to_rfc2822(),1,99)]
            .into_iter().enumerate().map(|(n,(name,date,total,bytes))|Release {
                name:name.into(),date,poster:"poster".into(),group:"alt.binaries.test".into(),
                articles:vec![Article {filename:Some("video.mkv".into()),subject:format!("video.mkv yEnc (1/{total})"),
                    message_id:format!("stats-{n}@example.invalid"),part:Some(1),total_parts:Some(total),bytes,..Default::default()}],..Default::default()
            }).collect::<Vec<_>>();
        assert_eq!(pg::release_writer::save(&mut client,&releases,1).unwrap().releases,3);
        client.execute("insert into release_category(release_id,category) select id,2040 from releases",&[]).unwrap();
        let id:i64=client.query_one("select id from releases where name='Current.Complete.Release'",&[]).unwrap().get(0);
        let title:i64=client.query_one("insert into ext_predb_titles(title,source,added_at) values('Current.Complete.Release',1,$1) returning id",&[&now.timestamp()]).unwrap().get(0);
        client.execute("insert into predb_match(release_id,title_id,kind,category,matched_at) values($1,$2,1,'<fixture>', $3)",&[&id,&title,&now.timestamp()]).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/stats/stored",&HashMap::new(),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("<td class=\"name\">Movies</td><td class=\"num\">2</td>"),"{}",response.body);
            assert!(response.body.contains("Current.Complete.Release")&&response.body.contains("&lt;fixture&gt;"));
            assert!(!response.body.contains("Old.Release")&&!response.body.contains("some shards could not be read"));
        }
        client.execute("update releases set posted_clock=null where id=$1",&[&id]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(dispatch(&view,"/ui/stats/stored",&HashMap::new(),"http://example.invalid","fixture-key").is_err(),"unknown posting time must not become a fabricated day count");
    }
}


#[cfg(test)]
mod visual_related_tests {
    use super::*;
    use crate::parser::{Article,Release};
    use std::time::{Duration,Instant};

    fn catalog(client:&mut postgres::Client,names:&[&str])->Vec<i64> {
        for (n,name) in names.iter().enumerate() {
            let stats=pg::release_writer::save(client,&[Release {name:(*name).into(),poster:"fixture poster".into(),group:"alt.binaries.test".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),
                    subject:"video.mkv yEnc (1/1)".into(),message_id:format!("<visual-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],
                ..Default::default()}],1).unwrap();
            assert_eq!((stats.releases,stats.files,stats.parts),(1,1,1));
        }
        names.iter().map(|name|client.query_one("select id from releases where name=$1",&[name]).unwrap().get(0)).collect()
    }
    fn picture(client:&mut postgres::Client,id:i64,position:i64,sha:&str,exif:Option<&str>,document:Option<&str>) {
        client.execute("insert into release_images(release_id,position,file,sha256,format,width,height,bytes,exif_id,document_id)
            values($1,$2,'cover.jpg',$3,'jpeg',10,10,17,$4,$5)",&[&id,&position,&sha,&exif,&document]).unwrap();
    }
    fn still(client:&mut postgres::Client,id:i64,position:i64,at:Option<i64>,hash:u64) {
        client.execute("insert into release_stills(release_id,position,at_ms,sha256,width,height,phash)
            values($1,$2,$3,$4,10,10,$5)",&[&id,&position,&at,&"a".repeat(64),&(hash as i64)]).unwrap();
    }
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and bounded native handler admission"]
    fn same_picture_handler_keeps_strongest_tie_and_order_without_lending_or_asset_reads() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        let ids=catalog(&mut client,&["Own.Picture.Release","Exif.Linked.Release","Bytes.Linked.Release","Document.Linked.Release","Unrelated.Picture.Release"]);
        let a="a".repeat(64);let b="b".repeat(64);let c="c".repeat(64);
        picture(&mut client,ids[0],0,&a,Some("same-exif"),Some("same-document"));
        picture(&mut client,ids[0],1,&b,None,None);
        picture(&mut client,ids[1],0,&c,Some("same-exif"),None);
        // First encountered by EXIF, but the later own picture proves identical
        // bytes too. This one must appear once with its strongest tie.
        picture(&mut client,ids[2],0,&b,Some("same-exif"),None);
        picture(&mut client,ids[3],0,&c,None,Some("same-document"));
        picture(&mut client,ids[4],0,&c,None,None);
        let before=client.query_one("select count(*),sum(bytes)::bigint from release_images",&[]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let args=HashMap::from([("id".into(),ids[0].to_string())]);
        let result=dispatch(&view,"/ui/samepicture",&args,"http://example.invalid","fixture-key").unwrap();
        assert_eq!(result.status,200);assert!(!result.failed);
        let body=result.body;
        let bytes=body.find("Bytes.Linked.Release").unwrap();let exif=body.find("Exif.Linked.Release").unwrap();let document=body.find("Document.Linked.Release").unwrap();
        assert!(bytes<exif&&exif<document,"{body}");assert_eq!(body.matches("Bytes.Linked.Release").count(),1);
        assert!(body.contains("the same file")&&body.contains("the same EXIF image id")&&body.contains("the same XMP document id"),"{body}");
        assert!(!body.contains("Unrelated.Picture.Release")&&!body.contains("Own.Picture.Release"),"{body}");
        assert!(!body.contains("fill=1")&&!body.contains("src=\"/ui/image"),"association lookup must not serve or lend bytes: {body}");
        drop(view);
        let after=client.query_one("select count(*),sum(bytes)::bigint from release_images",&[]).unwrap();
        assert_eq!((after.get::<_,i64>(0),after.get::<_,Option<i64>>(1)),(before.get::<_,i64>(0),before.get::<_,Option<i64>>(1)));
    }
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and bounded native handler admission"]
    fn same_film_handler_requires_two_distinct_own_stills_and_preserves_time_rules() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        let ids=catalog(&mut client,&["Own.Film.Release","Matched.Timed.Release","One.Still.Release","Too.Far.Release","Fallback.Position.Release","Repeated.One.Release","No.Match.Release"]);
        // High signed bit must survive PG bigint storage. The two own hashes
        // differ in all bits, so a repeated candidate cannot match both.
        let first=1u64<<63;let second=!first;
        still(&mut client,ids[0],25,Some(100_000),first);still(&mut client,ids[0],50,Some(200_000),second);
        still(&mut client,ids[1],1,Some(115_000),first^1);still(&mut client,ids[1],2,Some(185_000),second^1);
        still(&mut client,ids[2],25,Some(100_000),first);
        still(&mut client,ids[3],25,Some(115_001),first);still(&mut client,ids[3],50,Some(215_001),second);
        still(&mut client,ids[4],25,None,first);still(&mut client,ids[4],50,None,second);
        still(&mut client,ids[5],1,Some(100_000),first);still(&mut client,ids[5],2,Some(100_000),first);
        still(&mut client,ids[6],25,None,0);still(&mut client,ids[6],50,None,0);
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let args=HashMap::from([("id".into(),ids[0].to_string())]);
        let result=dispatch(&view,"/ui/samefilm",&args,"http://example.invalid","fixture-key").unwrap();assert_eq!(result.status,200);
        let body=result.body;assert!(body.contains("Not twins."),"{body}");
        assert!(body.find("Fallback.Position.Release").unwrap()<body.find("Matched.Timed.Release").unwrap(),"newest release first: {body}");
        for excluded in ["Own.Film.Release","One.Still.Release","Too.Far.Release","Repeated.One.Release","No.Match.Release"] {assert!(!body.contains(excluded),"{excluded}: {body}");}
        let empty=dispatch(&view,"/ui/samefilm",&HashMap::from([("id".into(),ids[2].to_string())]),"http://example.invalid","fixture-key").unwrap();
        assert_eq!(empty.status,200);assert!(empty.body.is_empty(),"one own still cannot establish same-film: {}",empty.body);
        assert_eq!(dispatch(&view,"/ui/samefilm",&HashMap::from([("id".into(),i64::MAX.to_string())]),"http://example.invalid","fixture-key").unwrap().status,404);
    }
}


#[cfg(test)]
mod native_continuation_tests {
    use super::*;
    use crate::parser::{Article,Release};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and bounded native handler admission"]
    fn native_results_render_real_first_middle_and_final_page_continuations() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        let releases=(0..105).map(|n|Release {name:format!("Paging.Fixture.Release.{n:03}"),group:"alt.binaries.test".into(),poster:"poster".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),
                message_id:format!("<paging-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}).collect::<Vec<_>>();
        let saved=pg::release_writer::save(&mut client,&releases,1).unwrap();assert_eq!((saved.releases,saved.files,saved.parts),(105,105,105));
        let ordered=client.query("select name from releases order by id desc",&[]).unwrap().into_iter().map(|row|row.get::<_,String>(0)).collect::<Vec<_>>();
        for (offset,count,next) in [(0usize,50,Some(50)),(50,50,Some(100)),(100,5,None)] {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let args=HashMap::from([("offset".into(),offset.to_string())]);
            for path in if offset==0{vec!["/","/ui/results"]}else{vec!["/ui/results"]} {
                let response=dispatch(&view,path,&args,"http://example.invalid","fixture-key").unwrap();assert_eq!(response.status,200);
                for (at,name) in ordered.iter().enumerate() {assert_eq!(response.body.contains(name),at>=offset&&at<offset+count,"offset{offset} {name}: {}",response.body);}
                match next {Some(next)=>assert!(response.body.contains(&format!("hx-get=\"/ui/results?offset={next}\"")),"{}",response.body),
                    None=>assert!(!response.body.contains("Load more"),"{}",response.body)}
                if path=="/" {assert!(response.body.contains("<footer class=\"site-footer\"")&&response.body.contains("https://www.patreon.com/cw/xbmc4lyfe"));}
                else if offset>0 {assert!(!response.body.contains("<html")&&!response.body.contains("<table"),"append pages remain table-row fragments");}
            }
            let total=dispatch(&view,"/ui/total",&args,"http://example.invalid","fixture-key").unwrap();assert_eq!(total.status,200);
            assert!(total.body.contains("105")&&!total.body.contains("Load more"),"{}",total.body);
        }
    }
}


#[cfg(test)]
mod kept_details_tests {
    use super::*;
    use crate::parser::{Article,Release};
    use std::time::{Duration,Instant};
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and bounded native handler admission"]
    fn kept_details_handler_uses_actual_id_lookup_and_tvdb_fallback_without_refreshing_cache() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        let mut ids=Vec::new();
        for n in 0..7 {
            let name=format!("Details.Fixture.Release.{n}");
            let stats=pg::release_writer::save(&mut client,&[Release {name:name.clone(),group:"alt.binaries.test".into(),poster:"poster".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),
                    message_id:format!("<kept-details-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1).unwrap();
            assert_eq!((stats.releases,stats.files,stats.parts),(1,1,1));
            ids.push(client.query_one("select id from releases where name=$1",&[&name]).unwrap().get::<_,i64>(0));
        }
        let media=[(2,Some(123),Some(42),None),(2,None,Some(42),None),(1,None,None,Some(99)),
            (1,None,Some(77),Some(100)),(1,Some(123),None,Some(101)),(1,Some(888),None,Some(102)),(2,None,None,Some(99))];
        for (id,(kind,tmdb,imdb,tvdb)) in ids.iter().zip(media) {
            client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id,imdb_id,tvdb_id) values($1,$2,$1,0,$3,$4,$5)",
                &[id,&(kind as i64),&tmdb.map(i64::from),&imdb.map(i64::from),&tvdb.map(i64::from)]).unwrap();
        }
        for (key,tmdb) in [("2:Some(42):None",Some(456i64)),("1:None:Some(99)",Some(567)),("1:Some(77):Some(100)",None),("1:None:Some(102)",Some(999))] {
            client.execute("insert into ext_media_details_lookups(lookup,tmdb_id,checked_at) values($1,$2,17)",&[&key,&tmdb]).unwrap();
        }
        for (kind,tmdb,title) in [(2i64,123i64,"Direct.Movie"),(2,456,"Mapped.Movie"),(1,-99,"TVDB.Only"),
            (1,123,"Direct.TV"),(1,-101,"Unused.TVDB"),(1,-102,"Fallback.TVDB"),(1,999,"Ignored.Lookup"),(2,-99,"Wrong.Kind.Fallback")] {
            client.execute("insert into ext_media_details(kind,tmdb_id,title,overview,imdb_score,fetched_at) values($1,$2,$3,'<kept & only>',81,1)",&[&kind,&tmdb,&title]).unwrap();
        }
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        let expected=[Some("Direct.Movie"),Some("Mapped.Movie"),Some("TVDB.Only"),None,Some("Direct.TV"),Some("Fallback.TVDB"),None];
        for (id,expected) in ids.iter().zip(expected) {
            let facts=view.listing_facts(&[*id]).unwrap().remove(id).unwrap();
            assert_eq!(facts.details.as_ref().and_then(|details|details.title.as_deref()),expected);
            let response=dispatch(&view,"/ui/details",&HashMap::from([("id".into(),id.to_string())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);
            match expected {Some(title)=>{assert!(response.body.contains(title),"{}",response.body);assert!(response.body.contains("&lt;kept &amp; only&gt;"),"{}",response.body);}
                None=>assert!(response.body.is_empty(),"negative or absent kept details remains absent: {}",response.body)}
        }
        drop(view);
        let row=client.query_one("select count(*),min(checked_at),max(checked_at) from ext_media_details_lookups",&[]).unwrap();
        assert_eq!((row.get::<_,i64>(0),row.get::<_,Option<i64>>(1),row.get::<_,Option<i64>>(2)),(4,Some(17),Some(17)),"read paths must not refresh retained lookup timestamps");
        let row=client.query_one("select count(*),min(fetched_at),max(fetched_at) from ext_media_details",&[]).unwrap();
        assert_eq!((row.get::<_,i64>(0),row.get::<_,Option<i64>>(1),row.get::<_,Option<i64>>(2)),(8,Some(1),Some(1)),"read paths must not refresh retained details");
        client.execute("update ext_media_details set overview=repeat('x',200000) where kind=2 and tmdb_id=456",&[]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {bytes:131_072,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.listing_facts(&[ids[1]]).is_err(),"mapped details must retain the existing pre-decode byte admission");
    }
}

#[cfg(test)]
mod advanced_native_query_tests {
    use super::*;
    use crate::parser::{Article,Release};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated native PostgreSQL and bounded handler admission"]
    fn actual_field_tag_rating_and_visibility_filters_page_the_same_kept_facts_the_row_shows() {
        let schema=pg::testing::Schema::new().expect("explicit isolated native schema");let mut client=schema.pool.get().unwrap();
        let inputs=[("Alpha.Movie.2020.1080p.WEB-DL-GRP","shared",17,1,Some(80i64)),("Beta.Movie.2021.2160p.BluRay-GRP","shared",23,1,Some(90)),
            ("Gamma.Movie.2022.1080p.WEB-DL-GRP","shared",29,3,Some(95)),("Delta.Movie.2023.1080p.WEB-DL-GRP","other",31,1,Some(100)),
            ("Spot.Movie.2024.1080p.WEB-DL-GRP","shared",37,1,Some(97)),("Removed.Movie.2025.1080p.WEB-DL-GRP","shared",41,1,Some(85)),
            ("Unrated.Movie.2026.1080p.WEB-DL-GRP","shared",43,1,None),("Lower.Movie.2027.1080p.WEB-DL-GRP","shared",47,1,Some(60))];
        let mut ids=Vec::new();
        for (n,(name,poster,bytes,total,score)) in inputs.iter().enumerate() {
            let saved=pg::release_writer::save(&mut client,&[Release {name:(*name).into(),poster:(*poster).into(),group:"alt.binaries.test".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),
                    message_id:format!("<advanced-{n}@example.invalid>"),part:Some(1),total_parts:Some(*total),bytes:*bytes,..Default::default()}],..Default::default()}],1).unwrap();
            assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));
            let id=client.query_one("select id from releases where name=$1",&[name]).unwrap().get::<_,i64>(0);ids.push(id);
            let tmdb=n as i64+1;
            client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) values($1,2,$1,0,$2)",&[&id,&tmdb]).unwrap();
            client.execute("insert into ext_media_details(kind,tmdb_id,imdb_score,fetched_at) values(2,$1,$2,1)",&[&tmdb,score]).unwrap();
        }
        client.execute("update media_release set tmdb_id=null,imdb_id=506 where release_id=$1",&[&ids[5]]).unwrap();
        client.execute("insert into ext_media_details_lookups(lookup,tmdb_id,checked_at) values('2:Some(506):None',6,1)",&[]).unwrap();
        client.execute("update media_release set kind=1,tvdb_id=108 where release_id=$1",&[&ids[6]]).unwrap();
        client.execute("update ext_media_details set kind=1 where kind=2 and tmdb_id=7",&[]).unwrap();
        client.execute("insert into ext_media_details(kind,tmdb_id,imdb_score,fetched_at) values(1,-108,100,1),(1,-107,60,1)",&[]).unwrap();
        client.execute("update media_release set kind=1,tvdb_id=107,tmdb_id=999 where release_id=$1",&[&ids[7]]).unwrap();
        for (at,mid) in [(4usize,"active-spot@example.invalid"),(5,"removed-spot@example.invalid")] {
            client.execute("insert into spots(release_id,message_id,kind,title,poster,spotter_id,created) values($1,$2,'movie','Spot title','spotter','account',1)",&[&ids[at],&mid]).unwrap();
        }
        client.execute("insert into spot_disposals(message_id,spotter,at) values('removed-spot@example.invalid','',2)",&[]).unwrap();
        let mut args=HashMap::from([("q".into(),"Movie".into()),("poster".into(),"shared".into()),("filters".into(),"1080p,Hide spots".into()),
            ("incomplete".into(),"hide".into()),("rated".into(),"70".into()),("sort".into(),"rating".into()),("dir".into(),"desc".into())]);
        for (direction,offset,wanted) in [("desc",0usize,vec![5usize,0]),("asc",0,vec![0,5])] {
            args.insert("dir".into(),direction.into());args.insert("offset".into(),offset.to_string());
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/results",&args,"http://example.invalid","fixture-key").unwrap();assert_eq!(response.status,200);
            for (n,(name,..)) in inputs.iter().enumerate(){assert_eq!(response.body.contains(name),wanted.contains(&n),"{direction} offset{offset} {name}: {}",response.body);}
            let positions=wanted.iter().map(|at|response.body.find(inputs[*at].0).unwrap()).collect::<Vec<_>>();assert!(positions.windows(2).all(|p|p[0]<p[1]));
            let total=dispatch(&view,"/ui/total",&args,"http://example.invalid","fixture-key").unwrap();assert!(total.body.contains("2 releases"),"{}",total.body);
        }
        {
            let search=Search::from_args(&args);
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let (rows,total)=view.field_search_with(&search.fields,&search.q,None,1,50,&search.tag_filters(),Some(SortBy::Rating),true,true,true,Some(70)).unwrap();
            assert_eq!(rows.iter().map(|row|row.id).collect::<Vec<_>>(),[ids[0]]);assert_eq!(total,2);
        }
        args.remove("rated");args.remove("incomplete");args.insert("filters".into(),"1080p".into());args.insert("sort".into(),"size".into());args.insert("dir".into(),"asc".into());args.remove("offset");
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        let response=dispatch(&view,"/ui/results",&args,"http://example.invalid","fixture-key").unwrap();
        let positions=[0usize,2,4,5,6,7].map(|at|response.body.find(inputs[at].0).unwrap());assert!(positions.windows(2).all(|p|p[0]<p[1]),"{}",response.body);
        assert!(!response.body.contains(inputs[1].0)&&!response.body.contains(inputs[3].0));
        drop(view);
        for n in 0..51 {
            let name=format!("Paged{n:02}.Movie.2020.1080p.WEB-DL");
            pg::release_writer::save(&mut client,&[Release {name:name.clone(),poster:"shared".into(),group:"alt.binaries.test".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),
                    message_id:format!("<paged-facet-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1).unwrap();
            let id:i64=client.query_one("select id from releases where name=$1",&[&name]).unwrap().get(0);
            client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) values($1,2,$1,0,1)",&[&id]).unwrap();
        }
        client.execute("update ext_media_details set imdb_score=90 where kind=2 and tmdb_id=1",&[]).unwrap();
        args.insert("rated".into(),"70".into());args.insert("incomplete".into(),"hide".into());args.insert("filters".into(),"1080p,Hide spots".into());
        args.insert("sort".into(),"rating".into());args.insert("dir".into(),"desc".into());args.insert("offset".into(),"50".into());
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        let response=dispatch(&view,"/ui/results",&args,"http://example.invalid","fixture-key").unwrap();
        assert!(response.body.contains("Paged00.Movie.")&&response.body.contains(inputs[0].0)&&response.body.contains(inputs[5].0),"{}",response.body);
        for n in 1..51 {assert!(!response.body.contains(&format!("Paged{n:02}.Movie.")));}
        assert!(!response.body.contains("Load more"));
    }
}

#[cfg(test)]
mod advanced_native_word_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,Backend,StorageConfig},word_index::{WordIndex,native::Budget as WordBudget}};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires isolated PostgreSQL, real native catalog namespace and owned Tantivy index"]
    fn filtered_word_search_retains_prefix_and_tag_matching_before_pg_row_hydration() {
        let schema=pg::testing::Schema::new().expect("explicit isolated native schema");let mut client=schema.pool.get().unwrap();
        client.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))",&[&schema.name]).unwrap();
        let root=tempfile::tempdir().unwrap();let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
        let storage=StorageConfig {backend:Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:4,allow_plaintext:true};
        let version=pg::migrate::version(&mut client,&schema.name).unwrap();let epoch=pg::stage::open_epoch(&mut client,&storage,&headers,version).unwrap();
        let inputs=[("Alpha.Movie.2020.1080p.WEB-DL",1,80i64),("Alphabet.Movie.2021.1080p.WEB-DL",3,95),
            ("AlphaBeta.Movie.2022.2160p.BluRay",1,90),("Unrelated.Movie.2023.1080p.WEB-DL",1,100)];
        let mut ids=Vec::new();
        for (n,(name,total,score)) in inputs.iter().enumerate() {
            assert_eq!(pg::release_writer::save(&mut client,&[Release {name:(*name).into(),poster:"poster".into(),group:"alt.binaries.test".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),
                    message_id:format!("<word-facet-{n}@example.invalid>"),part:Some(1),total_parts:Some(*total),bytes:17,..Default::default()}],..Default::default()}],1).unwrap().parts,1);
            let id=client.query_one("select id from releases where name=$1",&[name]).unwrap().get::<_,i64>(0);ids.push(id);
            let tmdb=n as i64+1;client.execute("insert into media_release(release_id,kind,title_key,quality,tmdb_id) values($1,2,$1,0,$2)",&[&id,&tmdb]).unwrap();
            client.execute("insert into ext_media_details(kind,tmdb_id,imdb_score,fetched_at) values(2,$1,$2,1)",&[&tmdb,score]).unwrap();
        }
        let directory=epoch.catalog().word_index_root(&headers).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(15)).unwrap();
        let mut source=pg::fresh_words::Search::open(&view).unwrap();let stamp=source.stamp().unwrap();let writer=WordIndex::open_native(&directory,stamp).unwrap();
        assert_eq!(writer.build_native(&mut source,WordBudget::default(),||true).unwrap().documents,4);drop(writer);
        let mut args=HashMap::from([("q".into(),"alph".into()),("filters".into(),"1080p".into()),("rated".into(),"70".into()),
            ("incomplete".into(),"hide".into()),("sort".into(),"rating".into()),("dir".into(),"desc".into())]);
        let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
        assert_eq!(rows.iter().map(|row|row.id).collect::<Vec<_>>(),[ids[0]]);assert_eq!((total.count,total.exact),(1,true));
        args.remove("rated");args.remove("incomplete");
        let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
        assert_eq!(rows.iter().map(|row|row.id).collect::<Vec<_>>(),[ids[1],ids[0]]);assert_eq!((total.count,total.exact),(2,true));
        args.insert("filters".into(),"2160p".into());
        let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
        assert_eq!(rows.iter().map(|row|row.id).collect::<Vec<_>>(),[ids[2]]);assert_eq!((total.count,total.exact),(1,true));
        drop(source);drop(view);
        args.remove("filters");args.insert("rated".into(),"70".into());args.insert("incomplete".into(),"hide".into());
        let index=WordIndex::read_native(&directory,stamp).unwrap();let (_,collector)=index.ranked_capacity(2,u64::MAX).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,bytes:1024*1024+collector+80},Instant::now()+Duration::from_secs(10)).unwrap();
        let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
        assert!(!total.exact,"unexamined ranked matches cannot become an exact filtered total");
        assert!(rows.len()<=2&&total.count<=2);assert!(total.describe().contains("At least")||total.describe().contains("newest releases"));
    }
}

#[cfg(test)]
mod bounded_native_string_sort_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,Backend,StorageConfig},word_index::{WordIndex,native::Budget as WordBudget}};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated PG, owned Tantivy build and bounded long-name resource qualification"]
    fn string_sorts_match_actual_words_without_collecting_discarded_long_names() {
        let schema=pg::testing::Schema::new().expect("explicit isolated native schema");let mut client=schema.pool.get().unwrap();
        client.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))",&[&schema.name]).unwrap();
        let root=tempfile::tempdir().unwrap();let headers=crate::config::HeaderStoreConfig {enabled:true,dir:root.path().to_str().unwrap().into()};
        let storage=StorageConfig {backend:Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:4,allow_plaintext:true};
        let version=pg::migrate::version(&mut client,&schema.name).unwrap();let epoch=pg::stage::open_epoch(&mut client,&storage,&headers,version).unwrap();
        for n in 0..192 {
            let name=if n<128{format!("ZZ.Bounded.{}.1080p.WEB-DL.{n}","x".repeat(65536))}else{format!("Bounded{:02}.Movie.2020.1080p.WEB-DL",n-128)};
            let total=if n<128{3}else{1};
            let saved=pg::release_writer::save(&mut client,&[Release {name,poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/1)".into(),message_id:format!("<bounded-sort-{n}@example.invalid>"),part:Some(1),total_parts:Some(total),bytes:17,..Default::default()}],..Default::default()}],1).unwrap();
            assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));
        }
        let directory=epoch.catalog().word_index_root(&headers).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:8,..Default::default()},Instant::now()+Duration::from_secs(60)).unwrap();
            let mut source=pg::fresh_words::Search::open(&view).unwrap();let stamp=source.stamp().unwrap();let index=WordIndex::open_native(&directory,stamp).unwrap();
            assert_eq!(index.build_native(&mut source,WordBudget {rows:8,..Default::default()},||true).unwrap().documents,192);
        }
        let mut args=HashMap::from([("q".into(),"bound".into()),("filters".into(),"1080p".into()),("incomplete".into(),"hide".into()),("sort".into(),"name".into()),("dir".into(),"asc".into())]);
        for (direction,offset,wanted) in [("asc",0,(0..50).collect::<Vec<_>>()),("asc",50,(50..64).collect()),("desc",0,(14..64).rev().collect())] {
            args.insert("dir".into(),direction.into());args.insert("offset".into(),offset.to_string());
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(15)).unwrap();
            let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
            assert_eq!(rows.iter().map(|row|row.name.clone()).collect::<Vec<_>>(),wanted.iter().map(|n|format!("Bounded{n:02}.Movie.2020.1080p.WEB-DL")).collect::<Vec<_>>());
            assert_eq!((total.count,total.exact),(64,true));assert!(rows.iter().all(|row|row.name.len()<128));
        }
        for sort in ["group","posted"] {
            args.insert("sort".into(),sort.into());args.insert("dir".into(),"desc".into());args.remove("offset");
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(15)).unwrap();
            let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();
            assert_eq!(rows.len(),50);assert_eq!((total.count,total.exact),(64,true));
            assert_eq!(rows[0].name,"Bounded63.Movie.2020.1080p.WEB-DL");
        }
        args.insert("q".into(),"bound missing".into());
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(15)).unwrap();
        let (rows,total)=query_using_headers(&view,&Search::from_args(&args),&headers).unwrap();assert!(rows.is_empty());assert_eq!((total.count,total.exact),(0,true));
    }
}

#[cfg(all(test,unix))]
mod native_asset_route_tests {
    use super::*;
    use sha2::{Digest,Sha256};
    use std::{io::Read,time::{Instant,Duration}};
    fn hex(bytes:&[u8])->String {bytes.iter().map(|b|format!("{b:02x}")).collect()}
    #[test]
    #[ignore="requires explicit isolated PG owner, genuine asset writer and Unix physical asset confinement"]
    fn actual_generated_poster_receipt_reaches_the_native_route_without_manifest_rewrite() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=pg::stage_queue_tests::runtime(&schema);
        let home=tempfile::tempdir().unwrap();let evidence=tempfile::tempdir().unwrap();let scratch=tempfile::tempdir().unwrap();
        let original=[&[0xff,0xd8,0xff,0xe0][..],b"original poster",&[0xff,0xd9]].concat();
        std::fs::create_dir(home.path().join("posters")).unwrap();std::fs::write(home.path().join("posters/retained.jpg"),&original).unwrap();
        let manifest=evidence.path().join("assets.jsonl");
        pg::testing::capture_asset_manifest(home.path(),&manifest,pg::testing::AssetCaptureLimits {min_files:1,max_files:16,max_file_bytes:1<<20,max_total_bytes:16<<20,read_bytes_per_second:32<<20}).unwrap();
        let manifest_bytes=std::fs::read(&manifest).unwrap();
        let selection=crate::config::NativeAssetsConfig {root:home.path().canonicalize().unwrap().to_str().unwrap().into(),manifest:manifest.canonicalize().unwrap().to_str().unwrap().into(),whole_sha256:hex(&Sha256::digest(&manifest_bytes))};
        let assets=pg::fresh_assets::AssetManifest::from_selection(&selection).unwrap();
        let grant=pg::worker_assets::Grant::open(&runtime.owner,Some(&selection)).unwrap().unwrap();
        let pool=crate::nzb_spool::Pool::new(scratch.path().into(),crate::nzb_spool::Limits {requests:2,per_request_bytes:1<<20,total_bytes:2<<20,free_bytes:0}).unwrap();
        let generated=[&[0xff,0xd8,0xff,0xe0][..],b"genuine generated poster",&[0xff,0xd9]].concat();
        for (id,path) in [(42,"/generated.jpg"),(43,"/retained.jpg")] {
            let details=crate::media::details::Details {kind:2,tmdb_id:id,poster:Some(path.into()),fetched_at:1,..Default::default()};
            runtime.owner.with(|client|pg::service_cache::details::put_in(client,&details)).unwrap();
        }
        assert_eq!(grant.produce_poster(2,42,"/generated.jpg",&generated).unwrap(),pg::worker_assets::ArtworkStatus::Kept);
        assert_eq!(std::fs::read(&manifest).unwrap(),manifest_bytes,"genuine new receipt needs no manifest rewrite");
        let mut client=schema.pool.get().unwrap();
        for (id,wanted) in [(42,&generated),(43,&original)] {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let args=HashMap::from([("kind".into(),"2".into()),("tmdb".into(),id.to_string())]);
            let mut response=asset_fragment(&view,"/ui/poster",&args,Some(&assets),&pool).unwrap();
            assert_eq!(response.status,200);assert_eq!(response.content_type,Some("image/jpeg"));
            assert!(response.headers.iter().any(|(name,value)|name=="X-Content-Type-Options"&&value=="nosniff"));
            let mut body=Vec::new();response.spool.as_mut().unwrap().read_to_end(&mut body).unwrap();assert_eq!(&body,wanted);
        }
        let args=HashMap::from([("kind".into(),"2".into()),("tmdb".into(),"42".into())]);
        std::fs::remove_file(home.path().join("posters/generated.jpg")).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let response=asset_fragment(&view,"/ui/poster",&args,Some(&assets),&pool).unwrap();assert_eq!(response.status,404);assert!(response.spool.is_none());
        }
        std::fs::write(home.path().join("posters/generated.jpg"),vec![b'X';generated.len()]).unwrap();
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            assert!(asset_fragment(&view,"/ui/poster",&args,Some(&assets),&pool).is_err(),"corruption is unavailable, never provider fallback");
        }
        std::fs::write(home.path().join("posters/generated.jpg"),&generated).unwrap();
        runtime.owner.with(|client|{client.execute("update ext_asset_files set source_digest=decode(repeat('00',32),'hex') where kind='posters' and name='generated.jpg'",&[])?;Ok(())}).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(asset_fragment(&view,"/ui/poster",&args,Some(&assets),&pool).is_err(),"an unbound PG receipt cannot grant file access");
        drop(view);runtime.owner.close();
    }
}

#[cfg(test)]
mod native_own_picture_tests {
    use super::*;
    use crate::parser::{Article,Release};
    use std::time::{Instant,Duration};
    #[test]
    #[ignore="requires explicit isolated PG, genuine empty-catalog owner bootstrap and own picture status rows"]
    fn own_picture_panels_and_status_read_actual_pg_rows_without_creating_requests() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PG required");let runtime=pg::stage_queue_tests::runtime(&schema);
        let names=["cover.jpg","queued.jpg","failed.jpg","offer.jpg"];
        let id=runtime.owner.with(|client|{
            let saved=pg::release_writer::save(client,&[Release {name:"Native.Picture.Release".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles:names.iter().enumerate().map(|(n,name)|Article {filename:Some((*name).into()),subject:format!("{name} yEnc (1/1)"),message_id:format!("<picture-{n}@example.invalid>"),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}).collect(),..Default::default()}],1)?;
            assert_eq!((saved.releases,saved.files,saved.parts),(1,4,4));
            let id:i64=client.query_one("select id from releases",&[])?.try_get(0)?;
            let metadata=serde_json::to_string(&crate::images::jpeg::JpegInfo {comment:Some("kept <comment>".into()),..Default::default()})?;
            client.execute("insert into release_images(release_id,position,file,sha256,format,width,height,bytes,meta) values($1,1,'cover.jpg',$2,'jpeg',640,360,17,$3)",&[&id,&"a1".repeat(32),&metadata])?;
            client.execute("insert into release_stills(release_id,position,at_ms,sha256,width,height) values($1,10,12345,$2,640,360)",&[&id,&"b2".repeat(32)])?;
            let bit=crate::postprocess::step::mask(crate::images::step::IMAGES_STEP);
            client.execute("insert into pp_state(release_id,net_wanted,net_left) values($1,true,$2)",&[&id,&bit])?;
            client.execute("insert into picture_requests(release_id,file,requested_at,done_at,outcome) values($1,'queued.jpg',1,null,null),($1,'failed.jpg',2,3,'not a picture <bad>')",&[&id])?;Ok(id)
        }).unwrap();
        {
            let mut client=schema.pool.get().unwrap();let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),"cover.jpg".into())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("SHOWN"),"a shown image needs no unrelated unavailable live ETA");
        }
        runtime.owner.close();let mut client=schema.pool.get().unwrap();
        for (name,expected) in [("cover.jpg","SHOWN"),("queued.jpg","QUEUED"),("failed.jpg","NOT SHOWN"),("offer.jpg","Picture requests are unavailable")] {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),name.into())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains(expected),"{}",response.body);assert!(!response.body.contains("hx-post="));
            if name=="queued.jpg" {assert!(response.body.contains("on hold: native processing"),"{}",response.body);}
            if name=="failed.jpg" {assert!(response.body.contains("&lt;bad&gt;")&&!response.body.contains("<bad>"));}
        }
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/release",&HashMap::from([("id".into(),id.to_string())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);for expected in ["<h2>Pictures</h2>","<h2>Stills</h2>","/ui/image?id=","/ui/still?id=","kept &lt;comment&gt;","SHOWN","QUEUED"] {assert!(response.body.contains(expected),"missing {expected}: {}",response.body);}
            assert!(!response.body.contains(r#"hx-post="/ui/picture""#));
            let missing=dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),"missing.jpg".into())]),"http://example.invalid","fixture-key").unwrap();assert_eq!(missing.status,404);
        }
        assert_eq!(client.query_one("select count(*) from picture_requests where release_id=$1",&[&id]).unwrap().get::<_,i64>(0),2,"readonly GET never enqueues");
        client.execute("update pp_state set net_left=0 where release_id=$1",&[&id]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        let response=dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),"queued.jpg".into())]),"http://example.invalid","fixture-key").unwrap();
        assert!(response.body.contains("NOT SHOWN")&&response.body.contains("isn't reading"),"{}",response.body);
        drop(view);
        let oversized=serde_json::to_string(&crate::images::jpeg::JpegInfo {comment:Some("x".repeat(256*1024)),..Default::default()}).unwrap();
        client.execute("update release_images set meta=$2 where release_id=$1",&[&id,&oversized]).unwrap();
        let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),"cover.jpg".into())]),"http://example.invalid","fixture-key").is_err(),"metadata allocation is admitted before JSON decode");
        drop(view);
        let live=pg::stage_queue_tests::runtime(&schema);
        let bit=crate::postprocess::step::mask(crate::images::step::IMAGES_STEP);
        let reader=live.owner.with(|client|{
            let saved=pg::release_writer::save(client,&[Release {name:"Native.Picture.Reader".into(),poster:"poster".into(),group:"alt.binaries.test".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles:vec![Article {filename:Some("other.jpg".into()),subject:"other.jpg yEnc (1/1)".into(),message_id:"<picture-reader@example.invalid>".into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()}],1)?;
            assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));
            let reader:i64=client.query_one("select id from releases where name='Native.Picture.Reader'",&[])?.try_get(0)?;
            client.execute("update release_images set meta=null where release_id=$1",&[&id])?;
            client.execute("update pp_state set net_left=$2 where release_id=$1",&[&id,&bit])?;
            client.execute("insert into pp_state(release_id,net_wanted,net_left) values($1,true,$2)",&[&reader,&bit])?;
            client.execute("insert into upload_link(release_id,lead_id) values($1,$2)",&[&id,&reader])?;
            client.execute("insert into picture_requests(release_id,file,requested_at,done_at,outcome) values($1,'queued.jpg',10,11,'new final <reason>'),($1,'unrelated.jpg',12,null,null)",&[&reader])?;Ok(reader)
        }).unwrap();assert_ne!(reader,id);
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let read=view.processing(id).unwrap().unwrap();assert!(view.processing_pace(read.owner.as_ref(),read.now).is_err(),"original older request has deliberately unavailable live observation");
        }
        {
            let view=ReadView::open(&mut client,&schema.name,pg::fresh_reader::Budget {rows:100,..Default::default()},Instant::now()+Duration::from_secs(10)).unwrap();
            let response=dispatch(&view,"/ui/picture",&HashMap::from([("id".into(),id.to_string()),("name".into(),"queued.jpg".into())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("NOT SHOWN")&&response.body.contains("new final &lt;reason&gt;"),"{}",response.body);
            let response=dispatch(&view,"/ui/release",&HashMap::from([("id".into(),id.to_string())]),"http://example.invalid","fixture-key").unwrap();
            assert_eq!(response.status,200);assert!(response.body.contains("new final &lt;reason&gt;"));assert!(!response.body.contains("QUEUED"));
        }
        live.owner.close();
    }
}
