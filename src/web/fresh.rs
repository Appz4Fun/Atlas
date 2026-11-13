//! Actual fresh-catalog GET dispatch. Static assets/login keep the common
//! outer path; signed-in reads never fall through to a legacy SQLite loader.
use super::*;
use anyhow::{Result,ensure,Context};
use crate::{pg::{self,fresh_reader::ReadView},word_index::native::{self,SearchSource}};

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
            if path=="/ui/total" {return Ok(html(200,exact_html(usize::try_from(total)?)));}
            let ids:Vec<_>=rows.iter().map(|row|row.id).collect();let mut facts=view.listing_facts(&ids)?;
            let mut body=String::new();
            for row in &rows {
                let facts=facts.remove(&row.id).context("native result facts disappeared")?;
                body.push_str(&row_from_facts(row,ResultFacts {category:facts.category,password:facts.password.as_ref(),media:facts.media.as_ref(),
                    details:facts.details.as_ref().map(ResultDetails::from),poster:None,recovery:None,copies:None,spot:None,whole:None,repair:false}));
            }
            let next=search.offset.checked_add(PAGE_SIZE).context("native result page offset overflow")?;
            if u64::try_from(next)?<total {body.push_str(&search.load_more());}
            let rows_html=if body.is_empty(){"<p class=\"note dim\">No releases match.</p>".to_owned()}else{table(&search,&body)};
            let content=format!("{}<p class=\"results-summary\">{} releases</p>{rows_html}",search.filter_links(),thousands(i64::try_from(total)?));
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
        "/ui/poster"|"/ui/cover"|"/ui/still"|"/ui/image"=>{
            let Some(request)=view.asset_request(path,args)? else{return Ok(ApiResponse::not_found())};
            let assets=assets.context("retained asset manifest/root has not been admitted")?;
            let Some(body)=assets.read(&request,&crate::api::fresh::spools()?)? else{return Ok(ApiResponse::not_found())};
            let mut response=ApiResponse::xml(200,String::new());response.content_type=Some(request.content_type);response.spool=Some(body);
            response.headers=vec![("Cache-Control".into(),"private, max-age=86400".into()),("X-Content-Type-Options".into(),"nosniff".into()),
                ("Content-Security-Policy".into(),"default-src 'none'; img-src 'self'; sandbox".into())];Ok(response)
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
        "Related postings, password hints, retained artwork and related metadata lookups are unavailable in the native reader.");
    let inputs=crate::web_meta::MetaInputs {release:&row,names:Some(&names),inner:&inner,contents:&contents,probe:found.as_ref()};
    let metadata=view.own_metadata(&inputs)?;
    let meta=crate::web_meta::meta_from_facts(&inputs,&metadata);
    let nzb_files=nzb_files_html(id,&contents,&HashMap::new(),&crate::web_settings::csrf_token(key));
    let files=inner_files_html(&inner,false);
    Ok(release_page_from_facts(&row,&shown,raw.as_deref(),&tags,&facts,listing.category,
        ReleaseSections {fill:String::new(),warning,spot:String::new(),nfo,images:String::new(),stills:String::new(),copies:String::new(),meta,probe,nzb_files,files}))
}

fn query(view:&ReadView<'_>,search:&Search)->Result<(Vec<ReleaseRow>,u64)> {
    if view.count()?==0{return Ok((Vec::new(),0));}
    ensure!(search.min_score.is_none() && !search.hide_incomplete && !search.hides_spots() && !search.filters.contains(&HIDE_COPIES),
        "fresh native advanced filtering is not yet connected");
    if search.fields.any() && search.fields!=(Fields {group:search.fields.group.clone(),..Fields::default()}) {
        ensure!(search.filters.is_empty()&&search.sort.is_none(),"native field tag/sort window is not yet connected");
        return view.field_search(&search.fields,&search.q,parse_filter(&search.cat).as_ref(),search.offset,PAGE_SIZE as usize);
    }
    if search.q.is_empty() && search.fields.group.is_empty() && search.filters.is_empty() && search.cat.is_empty() && search.sort.is_none() {
        return Ok((view.recent(search.offset,PAGE_SIZE as usize)?,view.visible_count()?));
    }
    let mut source=pg::fresh_words::Search::open(view)?;let stamp=source.stamp()?;
    let directory=view.word_index_root(&crate::config::header_store_config())?;
    let index=word_index::WordIndex::read_native(&directory,stamp)?;
    let by=match search.sort {Some(SortBy::Name)=>word_index::By::Name,Some(SortBy::Category)=>word_index::By::Category,
        Some(SortBy::Size)=>word_index::By::Size,Some(SortBy::Posted)=>word_index::By::Posted,Some(SortBy::Group)=>word_index::By::Group,
        Some(SortBy::Rating)=>anyhow::bail!("fresh native rating order is not connected"),None if search.q.is_empty()=>word_index::By::Newest,None=>word_index::By::Relevance};
    let group=(!search.fields.group.is_empty()).then_some(search.fields.group.as_str());
    let found=index.search_native_in_group(&mut source,&search.q,parse_filter(&search.cat).as_ref(),&search.tag_filters(),group,by,search.descending,
        usize::try_from(search.offset)?,PAGE_SIZE as usize,native::Budget {rows:100,..native::Budget::default()})?;
    Ok((found.rows,u64::try_from(found.total)?))
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
