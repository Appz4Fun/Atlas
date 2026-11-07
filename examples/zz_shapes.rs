// local probe, not committed
use std::io::BufRead;
fn main() {
    let mut by: std::collections::BTreeMap<(String, &'static str), (u64, i64, Vec<String>)> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let f: Vec<&str> = line.splitn(4, '\t').collect();
        let (g, s, b, m) = (f[0], f[1], f[2].trim().parse::<i64>().unwrap_or(0), f.get(3).copied().unwrap_or(""));
        if atlas::parser::parse_subject(s).is_some() {
            continue;
        }
        let a = atlas::parser::Article { subject: s.into(), message_id: m.into(), bytes: b, ..Default::default() };
        let mut u = atlas::parser::Unparsable::default();
        u.note(&a);
        let kind = if u.id_subject.0 > 0 {
            "id_subject"
        } else if u.token.0 > 0 {
            "token"
        } else {
            "other"
        };
        let e = by.entry((g.to_string(), kind)).or_default();
        e.0 += 1;
        e.1 += b;
        if e.2.len() < 8 && kind == "other" {
            e.2.push(s.chars().take(110).collect());
        }
    }
    for ((g, k), (n, b, ex)) in by {
        println!("{g} {k}: {n} articles, {} MB", b >> 20);
        for e in ex {
            println!("    {e}");
        }
    }
}
