//! Field searches retain the existing literal/ASCII-LIKE predicates. Ordinary
//! name search still belongs to Tantivy. Only bounded IDs/groups cross this
//! scan; selected display strings are separately admitted by ReadView::rows.
use super::fresh_reader::ReadView;
use crate::{category::CatFilter,field_search::{self,Fields},search::ReleaseRow};
use anyhow::{Result,Context,ensure};
use postgres::{types::ToSql,fallible_iterator::FallibleIterator};

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
    pub(crate) fn field_search(&self,fields:&Fields,words:&str,filter:Option<&CatFilter>,offset:i64,limit:usize)->Result<(Vec<ReleaseRow>,u64)> {
        ensure!(offset>=0&&limit>0&&limit<=self.budget.rows,"native field page exceeds admission");
        let bytes=fields.chosen().try_fold(4096u64,|sum,(_,s)|sum.checked_add(s.len() as u64*4).context("native field query extent overflow"))?;
        self.admit_owned(bytes.checked_add(words.len() as u64*8).context("native field query extent overflow")?)?;
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
        let sql=format!("select r.id,r.group_name from releases r where {} order by r.id desc",p.conditions.join(" and "));
        let mut ids=Vec::with_capacity(limit);let mut total=0u64;
        self.before_query()?;
        {
            let mut tx=self.tx.borrow_mut();
            let mut rows=tx.query_raw(&sql,p.values.iter().map(|v|v.as_ref() as &(dyn ToSql+Sync)))?;
            while let Some(row)=rows.next()? {
                self.check_deadline()?;let group:String=row.try_get(1)?;
                ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native field group exceeds admission");
                if crate::filters::group_blocked(&group){continue;}
                if total>=offset as u64&&ids.len()<limit{ids.push(row.try_get::<_,i64>(0)?);}
                total=total.checked_add(1).context("native field total overflow")?;
            }
        }
        Ok((self.rows(&ids)?,total))
    }
}
