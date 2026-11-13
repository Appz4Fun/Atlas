//! Pure decoding of model-supplied search plans.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub message: String,
    pub groups: Vec<String>,
    pub keywords: Vec<String>,
}

static THINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());
static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"```(?:json)?\s*").unwrap());
static OBJECT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{.*\}").unwrap());

/// Pull the json plan out of whatever the model said.
pub fn parse_plan(raw: &str) -> Plan {
    let cleaned = FENCE_RE.replace_all(&THINK_RE.replace_all(raw, ""), "").trim().to_string();

    let value = serde_json::from_str::<Value>(&cleaned)
        .ok()
        .or_else(|| OBJECT_RE.find(&cleaned).and_then(|m| serde_json::from_str(m.as_str()).ok()));

    let Some(v) = value else {
        return Plan { message: "couldnt understand ai response".into(), ..Default::default() };
    };

    let strings = |key: &str| -> Vec<String> {
        v[key].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default()
    };

    Plan {
        message: v["message"].as_str().unwrap_or("").to_string(),
        groups: strings("groups"),
        keywords: strings("keywords"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_parsing() {
        let p = parse_plan(
            "<think>hmm {not json}</think>\n```json\n{\"message\": \"ok\", \"groups\": [\"a.b\"], \"keywords\": [\"4k\"]}\n```",
        );
        assert_eq!(p, Plan { message: "ok".into(), groups: vec!["a.b".into()], keywords: vec!["4k".into()] });

        let p = parse_plan("sure! {\"groups\": [\"x\"]} hope that helps");
        assert_eq!(p.groups, vec!["x".to_string()]);
        assert!(p.keywords.is_empty());

        let p = parse_plan("no json here");
        assert_eq!(p.message, "couldnt understand ai response");
        assert!(p.groups.is_empty());
    }
}
