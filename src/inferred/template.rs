//! Streaming proof for one physical file's own partNofT message IDs.

/// This proves a command template, never a saved header or write authority.
/// The whole physical file must be streamed before consuming `missing`.
#[derive(Debug)]
pub(crate) struct Proof {total:i64,known:Vec<u8>,count:i64,tail:Option<String>,valid:bool}
impl Proof {
    pub(crate) fn new(total:i64)->Option<Self> {
        (2..=crate::parser::MAX_PARTS).contains(&total).then(||Self {total,known:vec![0;((total+7)/8) as usize],count:0,tail:None,valid:true})
    }
    pub(crate) fn push(&mut self,part:Option<i64>,mid:&[u8]) {
        if !self.valid {return;}
        let parsed=|| {
            // NNTP commands must not admit folded lines, CRLF or control bytes.
            // Oversize proof is ineligible, never a truncated command.
            if mid.len()>65500||mid.iter().any(u8::is_ascii_control) {return None;}
            let part=part.filter(|part|(1..=self.total).contains(part))?;
            let tail=crate::nzb::pattern_rest(std::str::from_utf8(mid).ok()?,part,self.total)?;
            Some((part,tail))
        };
        let Some((part,tail))=parsed() else {self.valid=false;return;};
        if let Some(expected)=&self.tail {if expected!=tail {self.valid=false;return;}}
        else {self.tail=Some(tail.into());}
        let at=(part-1) as usize;let mask=1u8<<(at%8);
        if self.known[at/8]&mask==0 {self.known[at/8]|=mask;self.count+=1;}
    }
    pub(crate) fn missing(&self)->impl Iterator<Item=i64>+'_ {
        let eligible=self.valid&&self.tail.is_some()&&self.count>=2&&self.count*2>=self.total;
        (1..=self.total).filter(move|part|eligible&&self.known[((*part-1)/8) as usize]&(1u8<<((*part-1)%8))==0)
    }
    pub(crate) fn message_id(&self,part:i64)->String {
        assert!((1..=self.total).contains(&part));
        format!("<part{part}of{}.{}>",self.total,self.tail.as_deref().expect("proved source template"))
    }
    #[cfg(test)]
    fn owned_bytes(&self)->usize {self.known.capacity()+self.tail.as_ref().map_or(0,String::capacity)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_template_stream_matches_legacy_file_own_inference() {
        for (total,parts) in [(6,vec![1,2,4,5]),(4,vec![1,2]),(6,vec![1,1,2]),(3,vec![1,2,3])] {
            let rows=parts.iter().map(|part|crate::search::ArticleRow {message_id:format!("<part{part}of{total}.token@example.invalid>"),filename:Some("own.bin".into()),part:Some(*part),total_parts:Some(total),bytes:Some(17),subject:None,poster:None,posted_date:None}).collect::<Vec<_>>();
            let expected=crate::nzb::inferred_segments(&rows).into_iter().flat_map(|file|file.rows).map(|row|row.message_id).collect::<Vec<_>>();
            let mut proof=Proof::new(total).unwrap();for row in &rows {proof.push(row.part,row.message_id.as_bytes());}
            assert_eq!(proof.missing().map(|part|proof.message_id(part)).collect::<Vec<_>>(),expected);
        }
    }
    #[test]
    fn native_template_rejects_conflicts_noncanonical_counters_and_command_injection() {
        for mids in [
            ["part1of4.a@example.invalid","part2of4.b@example.invalid"],
            ["part01of4.a@example.invalid","part2of4.a@example.invalid"],
            ["part1of4.a@example.invalid\r\nQUIT","part2of4.a@example.invalid\r\nQUIT"],
        ] {
            let mut proof=Proof::new(4).unwrap();proof.push(Some(1),mids[0].as_bytes());proof.push(Some(2),mids[1].as_bytes());
            assert_eq!(proof.missing().count(),0,"unproved or unsafe file cannot nominate any HEAD command");
        }
        assert!(Proof::new(250001).is_none());assert!(Proof::new(1).is_none());
    }
    #[test]
    fn native_template_proves_all_high_parts_without_retaining_article_ids() {
        let mut proof=Proof::new(250000).unwrap();
        for part in 1..=125000 {proof.push(Some(part),format!("part{part}of250000.token@example.invalid").as_bytes());}
        assert_eq!(proof.missing().count(),125000);assert_eq!(proof.missing().next(),Some(125001));
        assert_eq!(proof.message_id(250000),"<part250000of250000.token@example.invalid>");
        assert!(proof.owned_bytes()<=32768,"source proof retains only the fixed part bitmap and one tail");
    }
}
