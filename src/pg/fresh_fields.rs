//! Field searches retain the existing literal/ASCII-LIKE predicates. Ordinary
//! name search still belongs to Tantivy. Only bounded IDs/groups cross this
//! scan; selected display strings are separately admitted by ReadView::rows.
use super::fresh_reader::ReadView;
use crate::{category::CatFilter,field_search::{self,Fields},search::ReleaseRow};
use anyhow::{Result,Context,ensure};
use postgres::{types::ToSql,fallible_iterator::FallibleIterator};
use crate::web::SortBy;

struct Plan { conditions:Vec<String>,values:Vec<Box<dyn ToSql+Sync>> }
impl Plan {
    fn bind(&mut self,value:impl ToSql+Sync+'static)->String {
        self.values.push(Box::new(value));format!("${}",self.values.len())
    }
    fn equal(&mut self,column:&str,value:String) {let p=self.bind(value);self.conditions.push(format!("{column}={p}"));}
    fn like(&mut self,column:&str,value:String) {
        let p=self.bind(value.to_ascii_lowercase());
        // SQLite LIKE folds ASCII only. PostgreSQL ILIKE/lower would also fold
        // non-ASCII letters and would change literal legacy query semantics.
        self.conditions.push(format!("translate({column},'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz') collate \"C\" like {p} escape '\\'"));
    }
}
impl ReadView<'_> {
    /// Reserve the ranked ID vector and its PG selection descriptors before
    /// the word reader allocates. Keep room for the actual returned page.
    pub(crate) fn ranked_search_window(&self,index:&crate::word_index::WordIndex,maximum:usize)->Result<(usize,crate::word_index::native::Budget)> {
        let (window,collector)=index.ranked_capacity(maximum,self.remaining_owned().saturating_sub(1024*1024))?;
        self.admit_owned(collector.checked_add((window as u64).checked_mul(8).context("native ranked ID extent overflow")?).context("native collector extent overflow")?)?;
        Ok((window,crate::word_index::native::Budget {rows:window,bytes:collector,commit_bytes:collector}))
    }
    pub(crate) fn word_page_budget(&self,index:&crate::word_index::WordIndex,offset:usize,limit:usize)->Result<crate::word_index::native::Budget> {
        ensure!(limit>0&&limit<=self.budget.rows,"native word page exceeds admission");
        let collector=index.ranked_workspace(offset,limit)?;
        self.admit_owned(collector.checked_add((limit as u64).checked_mul(8).context("native ranked ID extent overflow")?).context("native collector extent overflow")?)?;
        Ok(crate::word_index::native::Budget {rows:limit,bytes:collector,commit_bytes:collector})
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sorted_word_search(&self,index:&crate::word_index::WordIndex,source:&mut impl crate::word_index::native::SearchSource,words:&str,filter:Option<&CatFilter>,tags:&[&str],group:Option<&str>,offset:i64,limit:usize,sort:SortBy,descending:bool,complete:bool,hide_spots:bool,min_score:Option<i64>)->Result<(Vec<ReleaseRow>,u64)> {
        ensure!(matches!(sort,SortBy::Name|SortBy::Group|SortBy::Posted),"native membership sort has no string field");
        let bytes=index.membership_workspace(100)?;self.admit_owned(bytes)?;
        let budget=crate::word_index::native::Budget {rows:100,bytes,commit_bytes:bytes};
        let matcher=index.native_membership(source,words,filter,tags,group,budget)?;
        let mut matches=|ids:&[i64]|matcher.matching_ids(ids);
        let fields=Fields {group:group.unwrap_or_default().to_owned(),..Fields::default()};
        self.select_search(&fields,"",filter,offset,limit,&[],Some(sort),descending,complete,hide_spots,min_score,None,Some(&mut matches))
    }
    pub(crate) fn field_search(&self,fields:&Fields,words:&str,filter:Option<&CatFilter>,offset:i64,limit:usize)->Result<(Vec<ReleaseRow>,u64)> {
        self.field_search_with(fields,words,filter,offset,limit,&[],None,true,false,false,None)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn field_search_with(&self,fields:&Fields,words:&str,filter:Option<&CatFilter>,offset:i64,limit:usize,tags:&[&str],sort:Option<SortBy>,descending:bool,complete:bool,hide_spots:bool,min_score:Option<i64>)->Result<(Vec<ReleaseRow>,u64)> {
        self.select_search(fields,words,filter,offset,limit,tags,sort,descending,complete,hide_spots,min_score,None,None)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn filter_ranked_search(&self,ids:&[i64],offset:i64,limit:usize,rating_order:bool,descending:bool,complete:bool,hide_spots:bool,min_score:Option<i64>)->Result<(Vec<ReleaseRow>,u64)> {
        self.select_search(&Fields::default(),"",None,offset,limit,&[],rating_order.then_some(SortBy::Rating),descending,complete,hide_spots,min_score,Some(ids),None)
    }
    #[allow(clippy::too_many_arguments)]
    fn select_search(&self,fields:&Fields,words:&str,filter:Option<&CatFilter>,offset:i64,limit:usize,tags:&[&str],sort:Option<SortBy>,descending:bool,complete:bool,hide_spots:bool,min_score:Option<i64>,ranked:Option<&[i64]>,mut membership:Option<&mut dyn FnMut(&[i64])->Result<Vec<i64>>>)->Result<(Vec<ReleaseRow>,u64)> {
        ensure!(offset>=0&&limit>0&&limit<=self.budget.rows,"native field page exceeds admission");
        let bytes=fields.chosen().try_fold(4096u64,|sum,(_,s)|sum.checked_add(s.len() as u64*4).context("native field query extent overflow"))?;
        let parameters=words.split_whitespace().count() as u64+32;
        self.admit_owned(bytes.checked_add(words.len() as u64*8).and_then(|n|n.checked_add(parameters*256)).context("native field query extent overflow")?)?;
        let mut p=Plan {conditions:vec!["(r.display_name is not null or(not r.is_obfuscated and(length(r.name)<16 or r.name collate \"C\" ~ '[^A-Za-z0-9]')))".into()],values:Vec::new()};
        if !fields.group.is_empty(){p.equal("r.group_name",fields.group.clone());}
        if !fields.poster.is_empty(){p.equal("r.poster",fields.poster.clone());}
        if !fields.email.is_empty(){p.like("r.poster",crate::search::like_query(&fields.email));}
        if let Ok(size)=fields.size.parse::<i64>(){let n=p.bind(size);p.conditions.push(format!("r.size={n}"));}
        else if let Some((low,high))=field_search::size_range(&fields.size){let a=p.bind(low);let b=p.bind(high);p.conditions.push(format!("r.size between {a} and {b}"));}
        else if !fields.size.is_empty(){p.conditions.push("false".into());}
        if !fields.team.is_empty(){let n=p.bind(field_search::team_pattern(&fields.team).to_ascii_lowercase());p.conditions.push(field_search::team_condition("translate(coalesce(r.display_name,r.name),'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz')",&n));}
        if !fields.crc.is_empty(){
            if let Ok(crc)=u32::from_str_radix(fields.crc.trim_start_matches("0x"),16){let n=p.bind(i64::from(crc));p.conditions.push(format!("exists(select 1 from release_probe probe where probe.release_id=r.id and probe.crc32={n})"));}
            else{p.conditions.push("false".into());}
        }
        if !fields.agent.is_empty(){let n=p.bind(fields.agent.clone());p.conditions.push(format!("exists(select 1 from release_probe probe where probe.release_id=r.id and probe.agent={n})"));}
        for word in words.split_whitespace(){p.like("coalesce(r.display_name,r.name)",crate::search::like_query(word));}
        if let Some(filter)=filter {let leaves=p.bind(filter.leaves.clone());let uncategorized=p.bind(filter.uncategorized);
            p.conditions.push(format!("(exists(select 1 from release_category c where c.release_id=r.id and c.category=any({leaves})) or({uncategorized} and not exists(select 1 from release_category c where c.release_id=r.id)))"));}
        let group_max=p.bind(crate::filters::MAX_GROUP_NAME as i32);p.conditions.push(format!("octet_length(r.group_name)<={group_max}"));
        if complete {p.conditions.push("r.complete".into());}
        if hide_spots {
            // The existing closed predicate is PostgreSQL-compatible; only
            // its legacy attached-main namespace differs in this RR view.
            let removed=crate::spotnet::store::disposed_sql("d","s","spotter_id",Some("created")).replace("main.","");
            p.conditions.push(format!("not exists(select 1 from spots s where s.release_id=r.id and not {removed})"));
        }
        let score="coalesce(kept.imdb_score,kept.tmdb_score,kept.trakt_score)";
        if let Some(min)=min_score {let n=p.bind(min);p.conditions.push(format!("{score}>={n}"));}
        let ranked_join=if let Some(ids)=ranked {
            ensure!(ids.len()<=crate::word_index::MAX_HITS,"native ranked selection exceeds original search horizon");
            if ids.is_empty(){return Ok((Vec::new(),0));}
            self.admit_owned((ids.len() as u64).checked_mul(32).context("native ranked descriptor extent overflow")?)?;
            {let mut unique=ids.to_vec();unique.sort_unstable();ensure!(unique.windows(2).all(|pair|pair[0]!=pair[1]),"duplicate native ranked public ID");}
            let input=p.bind(ids.to_vec());format!("join unnest({input}::bigint[]) with ordinality ranking(release_id,place) on ranking.release_id=r.id")
        }else{String::new()};
        let joins=if min_score.is_some()||sort==Some(SortBy::Rating) {r#"left join release_category category on category.release_id=r.id
            left join media_release media on media.release_id=r.id
            left join ext_media_details_lookups mapped on media.tmdb_id is null and mapped.lookup=(media.kind::text||':'||
                case when media.imdb_id is null then 'None' else 'Some('||media.imdb_id::text||')' end||':'||
                case when media.tvdb_id is null then 'None' else 'Some('||media.tvdb_id::text||')' end)
            left join lateral(select candidate.imdb_score,candidate.tmdb_score,candidate.trakt_score from(
                select d.imdb_score,d.tmdb_score,d.trakt_score,0 priority from ext_media_details d
                    where d.kind=media.kind and d.tmdb_id=coalesce(media.tmdb_id,mapped.tmdb_id)
                union all
                select d.imdb_score,d.tmdb_score,d.trakt_score,1 priority from ext_media_details d
                    where media.kind=1 and media.tvdb_id is not null and d.kind=media.kind and d.tmdb_id=-media.tvdb_id
                ) candidate order by priority limit 1) kept on true"#}else{"left join release_category category on category.release_id=r.id"};
        let joins=format!("{joins} {ranked_join}");
        let direction=if descending{"desc"}else{"asc"};
        let key=match sort {
            None=>"r.id".to_owned(),Some(SortBy::Name)=>"translate(coalesce(r.display_name,r.name),'ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz') collate \"C\"".into(),
            Some(SortBy::Group)=>"r.group_name collate \"C\"".into(),Some(SortBy::Size)=>"r.size".into(),Some(SortBy::Posted)=>if membership.is_some(){"nullif(r.posted_date,'') collate \"C\"".into()}else{"r.posted_clock".into()},Some(SortBy::Rating)=>score.into(),
            Some(SortBy::Category)=>{
                let mut ids=vec![crate::category::UNCATEGORIZED];for root in crate::category::TABLE {ids.push(root.id);ids.extend(root.subs.iter().map(|sub|sub.id));}
                ids.sort_unstable();ids.dedup();ids.sort_by_cached_key(|id|crate::category::label(*id).unwrap_or_default());
                format!("case coalesce(category.category,{}) {} else 0 end",crate::category::UNCATEGORIZED,ids.into_iter().enumerate().map(|(at,id)|format!("when {id} then {}",at+1)).collect::<Vec<_>>().join(" "))
            }
        };
        let tie=if ranked.is_some(){"ranking.place"}else{"r.id desc"};
        let order=if sort.is_none(){tie.to_owned()}else{format!("{key} {direction} nulls last,{tie}")};
        let conditions=p.conditions.join(" and ");
        let max_name:i64=if tags.is_empty(){0}else{
            self.before_query()?;
            self.tx.borrow_mut().query_one(&format!("select coalesce(max(octet_length(coalesce(r.display_name,r.name))),0)::bigint from releases r {joins} where {conditions}"),
                &p.values.iter().map(|v|v.as_ref() as &(dyn ToSql+Sync)).collect::<Vec<_>>())?.try_get(0)?
        };
        ensure!(max_name>=0,"negative native field tag extent");
        // One streamed name and parser workspace, never every discarded row.
        // Sorting/filtering only scalar facts needs no decoded candidate name.
        self.admit_owned(65536u64.checked_add(if tags.is_empty(){0}else{(max_name as u64).checked_mul(32).context("native tag workspace overflow")?}).context("native tag workspace overflow")?)?;
        let name=if tags.is_empty(){"null::text"}else{"coalesce(r.display_name,r.name)"};
        let sql=format!("select r.id,r.group_name,{name} from releases r {joins} where {conditions} order by {order}");
        let mut ids=Vec::with_capacity(limit);let mut total=0u64;let mut pending=Vec::with_capacity(100);
        let mut select=|pending:&mut Vec<i64>|->Result<()> {
            self.check_deadline()?;
            let matched=match membership.as_mut(){Some(test)=>test(pending)?,None=>pending.clone()};
            ensure!(matched.len()<=pending.len()&&matched.iter().all(|id|pending.contains(id)),"native membership escaped candidate batch");
            for id in pending.drain(..) {if !matched.contains(&id){continue;}
                if total>=offset as u64&&ids.len()<limit{ids.push(id);}
                total=total.checked_add(1).context("native field total overflow")?;
            }Ok(())
        };
        self.before_query()?;
        {
            let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw(&sql,p.values.iter().map(|v|v.as_ref() as &(dyn ToSql+Sync)))?;
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let group:String=row.try_get(1)?;
                ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native field group exceeds admission");
                if crate::filters::group_blocked(&group){continue;}
                if !tags.is_empty() {
                    let name:String=row.try_get(2)?;ensure!(name.len() as i64<=max_name,"native field tag extent changed");
                    let parsed=crate::release_tags::parse(&name);
                    if !tags.iter().all(|tag|crate::web::filter_matches(tag,&parsed)){continue;}
                }
                pending.push(row.try_get::<_,i64>(0)?);if pending.len()==100{select(&mut pending)?;}
            }
            select(&mut pending)?;
        }
        drop(select);
        Ok((self.rows(&ids)?,total))
    }
}
