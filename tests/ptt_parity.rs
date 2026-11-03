//! The port against what the Kodi add-on's PTT itself says, for names with a wide spread of shapes.

use atlas::ptt::{Value, parse};
use serde_json::{Value as Json, json};

#[test]
fn the_port_reads_names_as_ptt_does() {
    for line in include_str!("fixtures/ptt/expected.jsonl").lines() {
        let want: serde_json::Map<String, Json> = serde_json::from_str(line).unwrap();
        let name = want["t"].as_str().unwrap();
        let got = parse(name);
        assert_eq!(got.title, want["title"].as_str().unwrap(), "title of {name}");
        for (field, expected) in &want {
            if field == "t" || field == "title" || expected == &json!([]) {
                continue;
            }
            let value = match got.fields.get(field.as_str()) {
                Some(Value::Str(s)) => json!(s),
                Some(Value::Int(n)) => json!(n),
                Some(Value::Bool(b)) => json!(b),
                Some(Value::Strs(l)) => json!(l),
                Some(Value::Ints(l)) => json!(l),
                None => Json::Null,
            };
            assert_eq!(&value, expected, "{field} of {name}");
        }
    }
}
