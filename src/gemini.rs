//! Searching in plain words from the web ui: Gemini reads "4k dolby vision remuxes of dune" and answers with the
//! search page's own fields, which the page then searches with like any other search.
//!
//! The key lives in its own file beside config.toml, `gemini.json`, readable by the owner only. A page is only ever
//! told whether there is one and its last four characters.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::category::TABLE;

/// Google's name for its newest Flash model, soo the default doesn't age out.
pub const DEFAULT_MODEL: &str = "gemini-flash-latest";
const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";
const FILE: &str = "gemini.json";
/// What a question can be. Anything longer isn't a search.
pub const MAX_QUESTION: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub api_key: String,
    pub model: String,
    /// the key is `GEMINI_API_KEY`, none being saved
    pub from_env: bool,
}

impl Settings {
    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub fn model(&self) -> &str {
        if self.model.is_empty() { DEFAULT_MODEL } else { &self.model }
    }
}

fn file() -> PathBuf {
    crate::paths::app_dir().join(FILE)
}

/// The saved key and model, or `GEMINI_API_KEY` from the environment when none is saved.
pub fn load() -> Settings {
    let mut settings = load_saved();
    if settings.api_key.is_empty() {
        settings.api_key = std::env::var("GEMINI_API_KEY").map(|k| k.trim().to_string()).unwrap_or_default();
        settings.from_env = settings.has_key();
    }
    settings
}

/// Saves the key and model. A blank `api_key` keeps the saved one.
pub fn save(api_key: &str, model: &str) -> io::Result<Settings> {
    let mut settings = load_saved();
    if !api_key.trim().is_empty() {
        settings.api_key = api_key.trim().to_string();
    }
    settings.model = model.trim().to_string();
    write(&settings)?;
    Ok(load())
}

/// Removes the saved key, keeping the model.
pub fn forget() -> io::Result<()> {
    let mut settings = load_saved();
    settings.api_key.clear();
    write(&settings)
}

/// What the file holds, without the environment's key.
fn load_saved() -> Settings {
    let saved: Option<Value> = fs::read_to_string(file()).ok().and_then(|text| serde_json::from_str(&text).ok());
    let text = |key: &str| {
        saved.as_ref().and_then(|v| v.get(key)).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string()
    };
    Settings { api_key: text("api_key"), model: text("model"), from_env: false }
}

/// Written whole through a temp file that is the owner's alone from the start, never readable by others.
fn write(settings: &Settings) -> io::Result<()> {
    let target = file();
    let body = serde_json::to_string_pretty(&json!({"api_key": settings.api_key, "model": settings.model}))
        .map_err(io::Error::other)?;
    let tmp = crate::atomic::tmp_path(&target);
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&tmp)?;
    out.write_all(body.as_bytes())?;
    out.sync_all()?;
    drop(out);
    if crate::atomic::atomic_replace(&tmp, &target) {
        Ok(())
    } else {
        let _ = fs::remove_file(&tmp);
        Err(io::Error::other(format!("could not write {}", target.display())))
    }
}

/// "••••wxyz": enough to tell keys apart, never enough to use one.
pub fn masked(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let shown = if chars.len() >= 12 { 4 } else { 0 };
    let tail: String = chars[chars.len() - shown..].iter().collect();
    format!("{}{tail}", "•".repeat(8))
}

/// A search Gemini read out of a question, in the search page's own fields. Not yet checked: the page keeps only
/// what it knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub q: String,
    pub cat: String,
    pub filters: Vec<String>,
    pub group: String,
    pub poster: String,
    pub team: String,
    pub sort: String,
    pub dir: String,
    /// one sentence on how Gemini read the question
    pub note: String,
}

/// What the page can search on, for the prompt and the schema.
pub struct Choices<'a> {
    pub filters: &'a [&'a str],
    pub sorts: &'a [&'a str],
}

fn categories() -> String {
    let mut out = String::new();
    for root in TABLE {
        out.push_str(&format!("{} {}\n", root.id, root.name));
        for sub in root.subs {
            out.push_str(&format!("{} {} / {}\n", sub.id, root.name, sub.name));
        }
    }
    out
}

fn instructions(choices: &Choices) -> String {
    format!(
        r#"You turn a request for Usenet releases into a search on a Newznab indexer. Answer with JSON only.

Release names are scene style, like Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.TrueHD.Atmos-FGT or Show.Name.S02E05.1080p.WEB.h264-GROUP.

Fields:
- q: words that must all appear in the name, separated by spaces: title words, year, S01E02 or S01 for a season, codec or audio words. Leave out filler like "movies", "show", "find", "latest", "good". Leave out what a filter or the category already says. Empty to list the newest releases.
- cat: one category id from the list below, or "all". Pick the narrowest one that surely fits; "all" when unsure.
- filters: any of {filters}. 2160p is 4K/UHD. DV is Dolby Vision. HDR is HDR10 or HDR10+. Remux is an untouched disc copy. "Hide copies" when the request asks for no duplicates or reposts.
- group: a Usenet group, only when the request names one, like alt.binaries.teevee.
- poster: who posted it, only when the request names one.
- team: the release group after the dash, only when the request names one, like FLUX or NTb.
- sort: one of {sorts}. "relevance" when the request doesn't ask for an order. "posted" for newest or oldest, "size" for biggest or smallest, "rating" for best rated.
- dir: "desc" for newest, biggest, best; "asc" for oldest, smallest, A to Z.
- note: one short sentence telling the user how you read the request.

Categories:
{categories}"#,
        filters = choices.filters.join(", "),
        sorts = choices.sorts.join(", "),
        categories = categories(),
    )
}

fn schema(choices: &Choices) -> Value {
    let mut cats = vec!["all".to_string()];
    for root in TABLE {
        cats.push(root.id.to_string());
        cats.extend(root.subs.iter().map(|s| s.id.to_string()));
    }
    let text = || json!({"type": "STRING"});
    json!({
        "type": "OBJECT",
        "properties": {
            "q": text(),
            "cat": {"type": "STRING", "enum": cats},
            "filters": {"type": "ARRAY", "items": {"type": "STRING", "enum": choices.filters}},
            "group": text(),
            "poster": text(),
            "team": text(),
            "sort": {"type": "STRING", "enum": choices.sorts},
            "dir": {"type": "STRING", "enum": ["desc", "asc"]},
            "note": text(),
        },
        "required": ["q", "cat", "filters", "sort", "dir", "note"],
        "propertyOrdering": ["note", "q", "cat", "filters", "group", "poster", "team", "sort", "dir"],
    })
}

/// The generateContent request for a question.
pub fn request_body(question: &str, choices: &Choices) -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": instructions(choices)}]},
        "contents": [{"role": "user", "parts": [{"text": question}]}],
        "generationConfig": {
            "temperature": 0,
            "responseMimeType": "application/json",
            "responseSchema": schema(choices),
        },
    })
}

/// The plan in a generateContent reply, or what went wrong in words for the page.
pub fn parse_reply(reply: &Value) -> Result<Plan, String> {
    if let Some(message) = reply.pointer("/error/message").and_then(Value::as_str) {
        return Err(format!("Gemini said: {message}"));
    }
    let candidate =
        reply.pointer("/candidates/0").ok_or_else(|| match reply.pointer("/promptFeedback/blockReason") {
            Some(reason) => format!("Gemini would not answer ({})", reason.as_str().unwrap_or("blocked")),
            None => "Gemini sent no answer".to_string(),
        })?;
    let text: String = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    if text.trim().is_empty() {
        let why = candidate.get("finishReason").and_then(Value::as_str).unwrap_or("empty");
        return Err(format!("Gemini sent no answer ({why})"));
    }
    let start = text.find('{').unwrap_or(0);
    let end = text.rfind('}').map_or(text.len(), |i| i + 1);
    let v: Value = serde_json::from_str(&text[start..end.max(start)])
        .map_err(|_| "Gemini's answer was not a search".to_string())?;
    let field = |key: &str| v.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let filters = v
        .get("filters")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let cat = field("cat");
    let sort = field("sort");
    Ok(Plan {
        q: field("q"),
        cat: if cat == "all" { String::new() } else { cat },
        filters,
        group: field("group"),
        poster: field("poster"),
        team: field("team"),
        sort: if sort == "relevance" { String::new() } else { sort },
        dir: field("dir"),
        note: field("note"),
    })
}

fn base_url() -> String {
    std::env::var("ATLAS_GEMINI_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.into())
        .trim_end_matches('/')
        .to_string()
}

fn agent(base: &str, timeout: Duration) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        // the key goes in a header: a redirect must not carry it anywhere else
        .max_redirects(0)
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")));
    if base.starts_with("http://127.0.0.1") {
        b = b.proxy(None);
    }
    b.build().into()
}

/// One line of what an API key is made of: it goes in a header.
pub fn looks_like_key(key: &str) -> bool {
    (10..=200).contains(&key.len()) && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A model name as it goes in the url: letters, digits, dots and dashes only.
pub fn model_path(model: &str) -> Result<&str, String> {
    let model = model.trim().trim_start_matches("models/");
    if model.is_empty() || !model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err(format!("\u{201c}{model}\u{201d} is not a Gemini model name"));
    }
    Ok(model)
}

/// Asks Gemini what search `question` means.
pub fn plan(settings: &Settings, question: &str, choices: &Choices) -> Result<Plan, String> {
    if !settings.has_key() {
        return Err("there is no Gemini key: add one in Settings".into());
    }
    let span = crate::metrics::span("web.ask_gemini");
    let planned = ask(settings, question, choices);
    if planned.is_err() {
        span.failed();
    }
    planned
}

fn ask(settings: &Settings, question: &str, choices: &Choices) -> Result<Plan, String> {
    let model = model_path(settings.model())?;
    let base = base_url();
    let url = format!("{base}/v1beta/models/{model}:generateContent");
    let mut resp = agent(&base, Duration::from_secs(45))
        .post(&url)
        .header("x-goog-api-key", &settings.api_key)
        .send_json(request_body(question, choices))
        .map_err(|e| format!("could not reach Gemini: {e}"))?;
    let status = resp.status().as_u16();
    let reply: Value = resp.body_mut().read_json().unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(match parse_reply(&reply) {
            Err(said) if reply.get("error").is_some() => said,
            _ => format!("Gemini answered {status}"),
        });
    }
    parse_reply(&reply)
}

/// Whether Gemini takes the key and knows the model: a model lookup, which costs nothing.
pub fn check(settings: &Settings) -> Result<(), String> {
    let model = model_path(settings.model())?;
    let base = base_url();
    let mut resp = agent(&base, Duration::from_secs(15))
        .get(&format!("{base}/v1beta/models/{model}"))
        .header("x-goog-api-key", &settings.api_key)
        .call()
        .map_err(|e| format!("could not reach Gemini: {e}"))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let reply: Value = resp.body_mut().read_json().unwrap_or(Value::Null);
    Err(reply
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map_or_else(|| format!("Gemini answered {status}"), |m| format!("Gemini said: {m}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHOICES: Choices = Choices { filters: &["2160p", "DV"], sorts: &["relevance", "posted"] };

    #[test]
    fn a_key_is_never_shown_whole() {
        assert_eq!(masked("AIzaSyA-0123456789abcdWXYZ"), "••••••••WXYZ");
        assert_eq!(masked("short"), "••••••••");
        assert_eq!(masked(""), "••••••••");
    }

    #[test]
    fn the_request_asks_for_json_in_the_schema() {
        let body = request_body("4k dune", &CHOICES);
        assert_eq!(body.pointer("/contents/0/parts/0/text").unwrap(), "4k dune");
        assert_eq!(body.pointer("/generationConfig/responseMimeType").unwrap(), "application/json");
        let schema = body.pointer("/generationConfig/responseSchema").unwrap();
        assert_eq!(schema.pointer("/properties/filters/items/enum").unwrap(), &json!(["2160p", "DV"]));
        let cats = schema.pointer("/properties/cat/enum").unwrap().as_array().unwrap();
        assert!(cats.contains(&json!("2045")) && cats.contains(&json!("all")));
        let said = body.pointer("/systemInstruction/parts/0/text").unwrap().as_str().unwrap();
        assert!(said.contains("2045 Movies / UHD"), "{said}");
    }

    #[test]
    fn a_reply_becomes_a_plan() {
        let text = r#"{"note":"4K Dolby Vision copies of Dune","q":"dune","cat":"2045","filters":["2160p","DV"],"sort":"relevance","dir":"desc","team":" FLUX "}"#;
        let reply = json!({"candidates": [{"content": {"parts": [
            {"text": "thinking about it", "thought": true},
            {"text": text},
        ]}}]});
        let plan = parse_reply(&reply).unwrap();
        assert_eq!(plan.q, "dune");
        assert_eq!(plan.cat, "2045");
        assert_eq!(plan.filters, ["2160p", "DV"]);
        assert_eq!(plan.sort, "", "relevance is no sort");
        assert_eq!(plan.team, "FLUX");
        assert_eq!(plan.note, "4K Dolby Vision copies of Dune");

        let all =
            json!({"candidates": [{"content": {"parts": [{"text": "```json\n{\"q\":\"x\",\"cat\":\"all\"}\n```"}]}}]});
        assert_eq!(parse_reply(&all).unwrap().cat, "");
    }

    #[test]
    fn what_went_wrong_in_words() {
        let error = json!({"error": {"code": 400, "message": "API key not valid. Please pass a valid API key."}});
        assert_eq!(parse_reply(&error).unwrap_err(), "Gemini said: API key not valid. Please pass a valid API key.");
        let blocked = json!({"promptFeedback": {"blockReason": "SAFETY"}});
        assert_eq!(parse_reply(&blocked).unwrap_err(), "Gemini would not answer (SAFETY)");
        let empty = json!({"candidates": [{"finishReason": "MAX_TOKENS", "content": {"parts": []}}]});
        assert_eq!(parse_reply(&empty).unwrap_err(), "Gemini sent no answer (MAX_TOKENS)");
        let prose = json!({"candidates": [{"content": {"parts": [{"text": "no idea"}]}}]});
        assert!(parse_reply(&prose).is_err());
    }

    #[test]
    fn a_model_name_cannot_change_the_url() {
        assert_eq!(model_path("models/gemini-3.8-flash"), Ok("gemini-3.8-flash"));
        assert!(model_path("../../evil?x=").is_err());
        assert!(model_path("").is_err());
    }
}
