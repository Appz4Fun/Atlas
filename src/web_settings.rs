//! Settings in the browser: what the terminal's Settings menu edits, behind the same sign in.
//!
//! Every change goes through `config::save_config`, the path the terminal takes to write config.toml: the file is
//! replaced whole (`atomic::write_atomic`), and the indexer, which reads it every few seconds, applies it live or
//! rebuilds its pool when the servers, their connections or `max_unsaved_headers` changed (`bg_indexer::watch_config`).
//! Passwords and keys are write-only: a page never carries one, and an empty field keeps what is saved. Every form
//! carries a token tied to the session, soo a page elsewhere can't post a change with the browser's cookie.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::LazyLock;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::api::{ApiResponse, escape, parse_query};
use crate::config::{self, Config, INDEX_MODES, MAX_PARALLEL_GROUPS, UsenetServer, valid_index_mode};
use crate::gemini;
use crate::web::{attr, diag, html, masthead, page, same, session_token};

/// A callout on the page: level (`help`, `note`, `caution`), title, body.
pub(crate) type Msg = (&'static str, &'static str, String);

/// The form field and the request header a settings POST carries its token in. htmx sends the header on every
/// request from the page (`hx-headers`), a plain form the field.
const CSRF_FIELD: &str = "_csrf";
pub(crate) const CSRF_HEADER: &str = "X-CSRF-Token";
/// connections one server takes, as the terminal's prompt clamps it
const MAX_CONNECTIONS: u32 = 100;
/// how long after answering the api moves to a new address, soo the answer gets out first
const API_RESTART_AFTER: Duration = Duration::from_millis(800);

/// Drawn at start: a token outlives neither the process nor the session it was made for.
static CSRF_SECRET: LazyLock<[u8; 32]> = LazyLock::new(|| {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("os rng unavailable");
    bytes
});

/// The token the signed in session's forms carry: an HMAC over the session cookie's value, soo it says nothing
/// about the key and is no use to anyone without the cookie.
pub(crate) fn csrf_token(api_key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&*CSRF_SECRET).expect("any key length works");
    mac.update(b"atlas settings form\0");
    mac.update(session_token(api_key).as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a POST carries this session's token, in the form or the header.
pub(crate) fn csrf_ok(args: &HashMap<String, String>, header: Option<&str>, api_key: &str) -> bool {
    let want = csrf_token(api_key);
    args.get(CSRF_FIELD).map(String::as_str).into_iter().chain(header).any(|got| same(got.trim(), &want))
}

/// Which settings something outside atlas owns, soo the page shows them read-only instead of taking a change the
/// next start would undo: `ATLAS_NNTP_*` puts the servers (and with them the api address and the mode when their
/// variables are set) in the environment, and `ATLAS_MANAGED_KEYS` names config.toml keys written from outside.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Managed {
    pub servers: bool,
    pub index_mode: bool,
    pub parallel_groups: bool,
    pub max_unsaved_headers: bool,
    pub api_host: bool,
    pub api_port: bool,
    pub api_key: bool,
    pub groups: bool,
}

impl Managed {
    pub(crate) fn detect() -> Managed {
        let set = |name: &str| std::env::var(name).is_ok_and(|v| !v.is_empty());
        Managed::of(config::env_creds_active(), set, &config::managed_keys())
    }

    fn of(env_servers: bool, env_set: impl Fn(&str) -> bool, keys: &[String]) -> Managed {
        let named = |key: &str| keys.iter().any(|k| k == key);
        Managed {
            servers: env_servers || named("usenet_servers"),
            index_mode: (env_servers && env_set("ATLAS_INDEX_MODE")) || named("index_mode"),
            parallel_groups: named("parallel_groups"),
            max_unsaved_headers: named("max_unsaved_headers"),
            // the env branch of load_config always sets api_host, from ATLAS_API_HOST or the default
            api_host: env_servers || named("api_host"),
            api_port: (env_servers && env_set("ATLAS_API_PORT")) || named("api_port"),
            api_key: named("api_key"),
            groups: named("groups"),
        }
    }
}

/// The config as the running atlas sees it: the file, the keyring and the environment, as `load_config` reads them.
/// No file yet gives the defaults, soo the first server can be added here.
fn current() -> Config {
    config::load_config().unwrap_or_default()
}

/// The settings page, with a callout when a form was just answered.
pub(crate) fn settings_page(message: Option<Msg>, api_key: &str) -> String {
    let cfg = current();
    let managed = Managed::detect();
    let token = csrf_token(api_key);
    render(&cfg, &managed, &token, message)
}

fn render(cfg: &Config, managed: &Managed, token: &str, message: Option<Msg>) -> String {
    let message = message.map_or(String::new(), |(level, title, body)| diag(level, title, &body));
    page(
        "Atlas settings",
        &format!(
            r##"{masthead}<main class="detail settings" hx-boost="true" hx-headers="{headers}">
<p class="path"><a href="/">Atlas</a> › Settings</p><h1>Settings</h1>{message}
<nav class="settings-nav" aria-label="Settings sections"><a href="#servers">Usenet servers</a><a href="#indexing">Indexing</a><a href="#groups">Groups</a><a href="#api">API</a><a href="#keys">Keys</a></nav>
<p class="hint">Changes are written to <code>config.toml</code>, the file the terminal's Settings menu writes. The indexer reads it again within a few seconds.</p>
{servers}{indexing}{groups}{api}{keys}</main>"##,
            masthead = masthead(true),
            headers = attr(&format!(r#"{{"{CSRF_HEADER}":"{token}"}}"#)),
            servers = servers_section(cfg, managed, token),
            indexing = indexing_section(cfg, managed, token),
            groups = groups_section(cfg, managed, token),
            api = api_section(cfg, managed, token),
            keys = keys_section(token, cfg),
        ),
    )
}

fn hidden(name: &str, value: &str) -> String {
    format!("<input type=\"hidden\" name=\"{}\" value=\"{}\">", attr(name), attr(value))
}

fn csrf_input(token: &str) -> String {
    hidden(CSRF_FIELD, token)
}

/// A note where a setting is owned elsewhere.
fn managed_note(what: &str) -> String {
    format!(
        "<p class=\"managed\">{what} Changes made here would be undone on the next start, soo they are not taken.</p>"
    )
}

fn disabled(managed: bool) -> &'static str {
    if managed { " disabled" } else { "" }
}

// --- usenet servers ---------------------------------------------------------------------------------------------

fn servers_section(cfg: &Config, managed: &Managed, token: &str) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<section id="servers" class="panel"><h2>Usenet servers</h2>
<p>Tried in priority order, lowest first. Downloads in SABnzbd use the same order. A password is saved in the OS keyring where there is one, otherwise in <code>config.toml</code>, and never shown here again.</p>"#
    );
    if managed.servers {
        out.push_str(&managed_note(
            "The servers come from the <code>ATLAS_NNTP_*</code> environment, and <code>config.toml</code>'s are not saved while it is set.",
        ));
    }
    if cfg.servers.is_empty() {
        out.push_str("<p class=\"hint\">No server yet. Add one below to start indexing.</p>");
    }
    for (i, server) in cfg.servers.iter().enumerate() {
        out.push_str(&server_details(i, server, managed.servers, token));
    }
    if !managed.servers {
        let next_priority = cfg.servers.iter().map(|s| s.priority).max().unwrap_or(0) + 1;
        let blank = UsenetServer { priority: next_priority, ..UsenetServer::new("", "", "", 563) };
        let _ = write!(
            out,
            "<details class=\"server add\"><summary><span class=\"host\">Add a server</span></summary>{}</details>",
            server_form(None, &blank, token)
        );
    }
    out.push_str("</section>");
    out
}

/// One server: its facts on the summary line, the form to change it inside. Never the password.
fn server_details(i: usize, server: &UsenetServer, read_only: bool, token: &str) -> String {
    let mut tags = String::new();
    let _ = write!(
        tags,
        "<span>port {}</span><span>{}</span>",
        server.port,
        if server.use_ssl() { "SSL" } else { "plain" }
    );
    let _ = write!(tags, "<span>{} connections</span><span>priority {}</span>", server.connections(), server.priority);
    if !server.indexes() {
        tags.push_str("<span class=\"tag tag-ref\">downloads only</span>");
    }
    if server.priority == i64::MIN {
        tags.push_str("<span class=\"tag tag-ref\">from the environment</span>");
    }
    tags.push_str(if server.password.is_empty() {
        "<span class=\"tag tag-warn\">no password</span>"
    } else {
        "<span class=\"tag tag-ok\">password set</span>"
    });
    let body = if read_only { String::new() } else { server_form(Some(i), server, token) };
    format!(
        "<details class=\"server\"><summary><span class=\"host\">{}</span>{tags}</summary>{body}</details>",
        escape(&server.host)
    )
}

/// The add form (`at` None) or the edit form of server `at`.
fn server_form(at: Option<usize>, server: &UsenetServer, token: &str) -> String {
    let id = at.map_or("new".to_string(), |i| i.to_string());
    let field = |name: &str| format!("s{id}-{name}");
    let (password_hint, password_required) = if server.password.is_empty() {
        ("A password is needed", " required")
    } else {
        ("Leave empty to keep the saved password", "")
    };
    let position = at.map_or(String::new(), |i| hidden("i", &i.to_string()) + &hidden("was", &server.host));
    let actions = match at {
        Some(_) => {
            "<button class=\"button primary\" name=\"do\" value=\"edit\">Save</button><button class=\"button\" name=\"do\" value=\"remove\" formnovalidate>Remove</button>"
        }
        None => "<button class=\"button primary\" name=\"do\" value=\"add\">Add server</button>",
    };
    format!(
        r#"<form class="settings-form" method="post" action="/ui/settings/servers">{csrf}{position}
<label for="{host}">Host</label><input id="{host}" name="host" type="text" value="{host_value}" required autocomplete="off" spellcheck="false" placeholder="news.example.com">
<label for="{user}">Username</label><input id="{user}" name="username" type="text" value="{user_value}" autocomplete="off" spellcheck="false">
<label for="{pass}">Password</label><input id="{pass}" name="password" type="password" autocomplete="new-password" placeholder="{password_hint}"{password_required}>
<div class="field-row">
<label for="{port}">Port<input id="{port}" name="port" type="number" min="1" max="65535" value="{port_value}" required></label>
<label for="{conns}">Connections<input id="{conns}" name="connections" type="number" min="1" max="{max_conns}" value="{conns_value}" required></label>
<label for="{prio}">Priority<input id="{prio}" name="priority" type="number" value="{prio_value}" required></label>
</div>
<label class="check"><input name="ssl" type="checkbox"{ssl}> SSL (off on port 119)</label>
<label class="check"><input name="index" type="checkbox"{index}> Indexes headers (off keeps a block account for downloads and article lookups only)</label>
<div class="actions">{actions}</div></form>"#,
        csrf = csrf_input(token),
        host = field("host"),
        host_value = attr(&server.host),
        user = field("user"),
        user_value = attr(&server.username),
        pass = field("pass"),
        port = field("port"),
        port_value = server.port,
        conns = field("conns"),
        conns_value = server.connections(),
        max_conns = MAX_CONNECTIONS,
        prio = field("prio"),
        prio_value = server.priority,
        ssl = if server.use_ssl() { " checked" } else { "" },
        index = if server.indexes() { " checked" } else { "" },
    )
}

/// A server from the form, over `current` for an edit: the terminal's `prompt_server` rules.
fn server_from(args: &HashMap<String, String>, current: Option<&UsenetServer>) -> Result<UsenetServer, String> {
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    let blank = UsenetServer::new("", "", "", 563);
    let mut server = current.cloned().unwrap_or(blank);

    server.host = get("host").to_string();
    if server.host.is_empty() {
        return Err("The host can't be empty.".into());
    }
    if server.host.chars().any(|c| c.is_whitespace() || c == '/') {
        return Err("A host is a name or an address, without spaces or slashes.".into());
    }
    server.username = get("username").to_string();
    let password = args.get("password").map_or("", |v| v.trim());
    if !password.is_empty() {
        server.password = password.to_string();
    }
    if server.password.is_empty() {
        return Err("A password is needed.".into());
    }
    server.port = match get("port").parse::<u16>() {
        Ok(port) if port > 0 => port,
        _ => return Err("The port is a number from 1 to 65535.".into()),
    };
    server.ssl = Some(server.port != 119 && args.contains_key("ssl"));
    server.connections = match get("connections").parse::<u32>() {
        Ok(n) if (1..=MAX_CONNECTIONS).contains(&n) => Some(n),
        _ => return Err(format!("Connections is a number from 1 to {MAX_CONNECTIONS}.")),
    };
    server.priority = match get("priority").parse::<i64>() {
        Ok(p) if p != i64::MIN => p,
        _ => return Err("The priority is a whole number; lower is tried first.".into()),
    };
    // on is the default and keeps the file as it was; off is written
    server.index = if args.contains_key("index") { None } else { Some(false) };
    Ok(server)
}

/// The server form's change, put on `cfg`. Ok is what to say, Err why nothing changed.
fn apply_servers(cfg: &mut Config, args: &HashMap<String, String>, managed: &Managed) -> Result<String, String> {
    if managed.servers {
        return Err("The servers come from the ATLAS_NNTP_* environment: change them there.".into());
    }
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    let position = |cfg: &Config| -> Result<usize, String> {
        let i: usize = get("i").parse().map_err(|_| "Which server?".to_string())?;
        let server = cfg.servers.get(i).ok_or("That server is gone. Reload the page.")?;
        if !server.host.eq_ignore_ascii_case(get("was")) {
            return Err("The server list changed. Reload the page.".into());
        }
        Ok(i)
    };
    let said = match get("do") {
        "add" => {
            let server = server_from(args, None)?;
            let said = format!("{} is added.", server.host);
            cfg.servers.push(server);
            said
        }
        "edit" => {
            let i = position(cfg)?;
            let server = server_from(args, Some(&cfg.servers[i]))?;
            let said = format!("{} is saved.", server.host);
            cfg.servers[i] = server;
            said
        }
        "remove" => {
            let i = position(cfg)?;
            if cfg.servers.len() == 1 {
                return Err("The last server stays. Add another first.".into());
            }
            let gone = cfg.servers.remove(i);
            format!("{} is removed.", gone.host)
        }
        _ => return Err("What to do with the server?".into()),
    };
    let twice = cfg.duplicate_servers();
    if !twice.is_empty() {
        return Err(format!(
            "{} is listed twice with the same login, which would open double the account's connections. A second account on one host needs its own key in config.toml.",
            twice.join(", ")
        ));
    }
    cfg.sort_servers();
    Ok(format!("{said} The indexer reconnects with the new list within a few seconds."))
}

// --- indexing ---------------------------------------------------------------------------------------------------

fn indexing_section(cfg: &Config, managed: &Managed, token: &str) -> String {
    let mut notes = String::new();
    if managed.index_mode {
        notes.push_str(&managed_note("The mode comes from <code>ATLAS_INDEX_MODE</code>."));
    }
    if managed.parallel_groups {
        notes.push_str(&managed_note("<code>parallel_groups</code> is written from outside atlas."));
    }
    if managed.max_unsaved_headers {
        notes.push_str(&managed_note("<code>max_unsaved_headers</code> is written from outside atlas."));
    }
    let modes: String = INDEX_MODES
        .iter()
        .map(|mode| {
            let selected = if *mode == cfg.index_mode { " selected" } else { "" };
            format!("<option value=\"{mode}\"{selected}>{mode}</option>")
        })
        .collect();
    let parallel = cfg.parallel_groups.map_or(String::new(), |n| n.to_string());
    let unsaved = cfg.max_unsaved_headers.map_or(String::new(), |n| n.to_string());
    format!(
        r#"<section id="indexing" class="panel"><h2>Indexing</h2>
<p><b>dynamic</b> takes new headers first and backfills when caught up, <b>live</b> only new ones, <b>backfill</b> only old ones. The mode applies at once; groups at once and the header cap restart the indexer's connections.</p>{notes}
<form class="settings-form" method="post" action="/ui/settings/indexing">{csrf}
<label for="index-mode">Mode</label><select id="index-mode" name="index_mode"{mode_off}>{modes}</select>
<label for="parallel-groups">Groups indexed at once</label><input id="parallel-groups" name="parallel_groups" type="number" min="1" max="{max_parallel}" value="{parallel}" placeholder="worked out from the connections"{parallel_off}>
<p class="hint">Empty: one group per connection on each server. At most {max_parallel}.</p>
<label for="max-unsaved">Headers fetched ahead of saving</label><input id="max-unsaved" name="max_unsaved_headers" type="number" min="1" value="{unsaved}" placeholder="{default_unsaved}"{unsaved_off}>
<p class="hint">Empty: {default_unsaved}. Lower it when memory is tight, raise it when the writer keeps up and connections wait.</p>
<div class="actions"><button class="button primary">Save</button></div></form></section>"#,
        csrf = csrf_input(token),
        mode_off = disabled(managed.index_mode),
        max_parallel = MAX_PARALLEL_GROUPS,
        parallel_off = disabled(managed.parallel_groups),
        unsaved_off = disabled(managed.max_unsaved_headers),
        default_unsaved = config::DEFAULT_MAX_UNSAVED_HEADERS,
    )
}

/// An optional number from a form: empty is None, otherwise it has to be in `range`.
fn optional_number<T: std::str::FromStr + PartialOrd + Copy>(
    value: &str,
    range: std::ops::RangeInclusive<T>,
    what: &str,
) -> Result<Option<T>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<T>() {
        Ok(n) if range.contains(&n) => Ok(Some(n)),
        _ => Err(format!("{what} is a whole number, or empty.")),
    }
}

fn apply_indexing(cfg: &mut Config, args: &HashMap<String, String>, managed: &Managed) -> Result<String, String> {
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    // everything is checked before anything is taken, soo an error leaves the config as it was
    let mode = if managed.index_mode { cfg.index_mode.clone() } else { get("index_mode").to_string() };
    if !valid_index_mode(&mode) {
        return Err(format!("The mode is one of {}.", INDEX_MODES.join(", ")));
    }
    let parallel = if managed.parallel_groups {
        cfg.parallel_groups
    } else {
        optional_number(get("parallel_groups"), 1..=MAX_PARALLEL_GROUPS, "Groups indexed at once")?
    };
    let unsaved = if managed.max_unsaved_headers {
        cfg.max_unsaved_headers
    } else {
        optional_number(get("max_unsaved_headers"), 1..=u64::MAX, "Headers fetched ahead of saving")?
    };
    let mut changed = Vec::new();
    if mode != cfg.index_mode {
        changed.push(format!("mode {mode}"));
        cfg.index_mode = mode;
    }
    if parallel != cfg.parallel_groups {
        changed.push(parallel.map_or("groups at once worked out".to_string(), |n| format!("{n} groups at once")));
        cfg.parallel_groups = parallel;
    }
    if unsaved != cfg.max_unsaved_headers {
        changed.push(unsaved.map_or("default header cap".to_string(), |n| format!("{n} headers ahead")));
        cfg.max_unsaved_headers = unsaved;
    }
    Ok(if changed.is_empty() {
        "Nothing changed.".to_string()
    } else {
        format!("Saved: {}. The indexer picks it up within a few seconds.", changed.join(", "))
    })
}

// --- api --------------------------------------------------------------------------------------------------------

fn api_section(cfg: &Config, managed: &Managed, token: &str) -> String {
    let mut notes = String::new();
    if managed.api_host || managed.api_port {
        notes.push_str(&managed_note(
            "The address comes from <code>ATLAS_API_HOST</code> and <code>ATLAS_API_PORT</code> in the environment.",
        ));
    }
    let key = if managed.api_key {
        "The API key is written into <code>config.toml</code> from outside atlas, soo it isn't changed here."
    } else {
        "The API key is <code>api_key</code> in <code>config.toml</code>. Changing it there signs every browser out."
    };
    format!(
        r#"<section id="api" class="panel"><h2>Newznab API and this page</h2>
<p>Where the API, and with it this web UI, listens. <code>0.0.0.0</code> takes connections from other machines, <code>127.0.0.1</code> this one only. Saving a new address moves the API there within a second: sign in again at the new one.</p>{notes}
<form class="settings-form" method="post" action="/ui/settings/api">{csrf}
<div class="field-row">
<label for="api-host">Host<input id="api-host" name="api_host" type="text" value="{host}" required autocomplete="off" spellcheck="false"{host_off}></label>
<label for="api-port">Port<input id="api-port" name="api_port" type="number" min="1" max="65535" value="{port}" required{port_off}></label>
</div>
<p class="hint">{key}</p>
<div class="actions"><button class="button primary">Save</button></div></form></section>"#,
        csrf = csrf_input(token),
        host = attr(&cfg.api_host),
        host_off = disabled(managed.api_host),
        port = cfg.api_port(),
        port_off = disabled(managed.api_port),
    )
}

/// Ok: what to say, and whether the address changed (the api has to move).
fn apply_api(cfg: &mut Config, args: &HashMap<String, String>, managed: &Managed) -> Result<(String, bool), String> {
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    // everything is checked before anything is taken, soo an error leaves the config as it was
    let host = if managed.api_host {
        cfg.api_host.clone()
    } else {
        let host = get("api_host");
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c == '/') {
            return Err("The host is a name or an address, without spaces or slashes.".into());
        }
        host.to_string()
    };
    let port = if managed.api_port {
        cfg.api_port()
    } else {
        match get("api_port").parse::<u16>() {
            Ok(port) if port > 0 => port,
            _ => return Err("The port is a number from 1 to 65535.".into()),
        }
    };
    let moved = host != cfg.api_host || port != cfg.api_port();
    cfg.api_host = host;
    cfg.api_port = Some(port);
    Ok(if moved {
        (format!("The API moves to http://{}:{} in a moment. Sign in there.", cfg.api_host, cfg.api_port()), true)
    } else {
        ("Nothing changed.".to_string(), false)
    })
}

/// Stop the api and start it on the saved address, once the answer to the form is out. From its own thread: a
/// request's worker can't join itself.
fn restart_api_later() {
    std::thread::spawn(|| {
        std::thread::sleep(API_RESTART_AFTER);
        crate::api::stop();
        if let Some(mut fresh) = config::load_config() {
            crate::api::start(&mut fresh);
        }
    });
}

// --- keys -------------------------------------------------------------------------------------------------------

fn groups_section(cfg: &Config, managed: &Managed, token: &str) -> String {
    let mut out = String::from(
        "<section id=\"groups\" class=\"panel\"><h2>Groups</h2><p>These Usenet groups are indexed. A group name must be a real, non-blocked binary group.</p>",
    );
    if managed.groups {
        out.push_str(&managed_note("Groups are written by the container at startup."));
    }
    out.push_str("<ul class=\"settings-list\">");
    for group in &cfg.groups {
        let remove = if managed.groups {
            String::new()
        } else {
            format!(
                "<form method=\"post\" action=\"/ui/settings/groups\" class=\"inline\">{}<input type=\"hidden\" name=\"group\" value=\"{}\"><button class=\"button\" name=\"do\" value=\"remove\">Remove</button></form>",
                csrf_input(token),
                attr(group)
            )
        };
        let _ = write!(out, "<li><code>{}</code>{}</li>", escape(group), remove);
    }
    if !managed.groups {
        out.push_str(&format!(r###"</ul><form class="settings-form" method="post" action="/ui/settings/groups">{csrf}<label for="group-name">Add group<input id="group-name" name="group" required autocomplete="off" spellcheck="false"></label><button class="button primary" name="do" value="add">Add</button></form><h3>Find a provider group</h3><form class="settings-form" method="get" action="/ui/settings/groups/search" hx-get="/ui/settings/groups/search" hx-target="#group-results" hx-trigger="keyup changed delay:400ms"><label for="group-search">Search (at least three characters)<input id="group-search" name="q" autocomplete="off"></label></form><div id="group-results"></div></section>"###, csrf = csrf_input(token)));
    } else {
        out.push_str("</ul></section>");
    }
    out
}


/// Render the existing group picker from an already selected data source.
pub(crate) fn group_results(result:Result<Vec<String>,String>,api_key:&str)->String {
    let token = csrf_token(api_key);
    match result {
        Ok(groups) if groups.is_empty() => "<p class=\"hint\">No matching groups found.</p>".into(),
        Ok(groups) => groups.into_iter().map(|group| format!(r#"<div class="search-result"><code>{}</code><form method="post" action="/ui/settings/groups"><input type="hidden" name="_csrf" value="{}"><input type="hidden" name="group" value="{}"><button class="button" name="do" value="add">Add</button></form></div>"#, escape(&group), attr(&token), attr(&group))).collect(),
        Err(message) => format!("<p class=\"hint\">{}</p>", escape(&message)),
    }
}

fn keys_section(token: &str, cfg: &Config) -> String {
    let fields = [
        ("tmdb_api_key", "TMDB API key"),
        ("omdb_api_key", "OMDb API key"),
        ("tvdb_api_key", "TVDB API key"),
        ("tvdb_pin", "TVDB PIN"),
        ("trakt_client_id", "Trakt client ID"),
        ("opensubtitles_api_key", "OpenSubtitles API key"),
    ];
    let mut out = format!(
        "<section id=\"keys\" class=\"panel\"><h2>Keys</h2><p>Provider credentials are write-only. Empty fields keep their saved value; use a clear box to remove one.</p><form class=\"settings-form\" method=\"post\" action=\"/ui/settings/keys\">{}",
        csrf_input(token)
    );
    for (name, label) in fields {
        let current = match name {
            "tmdb_api_key" => !cfg.media.tmdb.api_key.is_empty(),
            "omdb_api_key" => !cfg.media.omdb.api_key.is_empty(),
            "tvdb_api_key" => !cfg.media.tvdb.api_key.is_empty(),
            "tvdb_pin" => !cfg.media.tvdb.pin.is_empty(),
            "trakt_client_id" => !cfg.media.trakt.client_id.is_empty(),
            _ => !cfg.media.opensubtitles.api_key.is_empty(),
        };
        let _ = write!(
            out,
            "<label>{label}<input type=\"password\" name=\"{name}\" autocomplete=\"off\"></label><label><input type=\"checkbox\" name=\"clear_{name}\"> clear {label} ({})</label>",
            if current { "saved" } else { "not set" }
        );
    }
    out.push_str("<button class=\"button primary\">Save provider keys</button></form>");
    out.push_str(&gemini_form(token));
    out.push_str("</section>");
    out
}

/// Ask AI's key and model, as before: the key is never shown back.
fn gemini_form(token: &str) -> String {
    let settings = gemini::load();
    let masked = escape(&gemini::masked(&settings.api_key));
    let status = if settings.from_env {
        format!(
            "Atlas uses the key in <code>GEMINI_API_KEY</code>, <code>{masked}</code>. A key saved here takes its place."
        )
    } else if settings.has_key() {
        format!("A key is saved: <code>{masked}</code>. Paste another to replace it.")
    } else {
        "No key yet. Make one in <a href=\"https://aistudio.google.com/apikey\" target=\"_blank\" rel=\"noreferrer noopener\">Google AI Studio</a>, then paste it here.".to_string()
    };
    let forget = if settings.has_key() && !settings.from_env {
        "<button class=\"button\" name=\"do\" value=\"forget\" formnovalidate>Remove key</button>"
    } else {
        ""
    };
    format!(
        r#"<h3 id="gemini">Ask AI with Gemini</h3>
<p>Ask AI, on the search page, sends what you type to Google's Gemini with the list of categories and filters, then searches with the fields Gemini sends back. Nothing from your index goes to Google.</p>
<p>{status}</p>
<form class="settings-form" method="post" action="/ui/settings">{csrf}
<label for="gemini-key">Gemini API key</label>
<input id="gemini-key" name="key" type="password" autocomplete="off" spellcheck="false" placeholder="{placeholder}">
<label for="gemini-model">Model</label>
<input id="gemini-model" name="model" type="text" value="{model}" placeholder="{default}" autocomplete="off" spellcheck="false">
<p class="hint">Leave it empty for <code>{default}</code>, which follows Google's newest Flash model.</p>
<div class="actions"><button class="button primary" name="do" value="save">Save</button>{forget}</div>
</form>"#,
        csrf = csrf_input(token),
        placeholder = if settings.has_key() { "Paste a new key to replace it" } else { "Paste your key" },
        model = attr(&settings.model),
        default = gemini::DEFAULT_MODEL,
    )
}

fn apply_gemini(args: &HashMap<String, String>) -> Msg {
    let get = |k: &str| args.get(k).map_or("", |v| v.trim());
    if get("do") == "forget" {
        return match gemini::forget() {
            Ok(()) => ("help", "the key is removed", "Ask AI is off until you add another.".to_string()),
            Err(e) => ("caution", "could not remove the key", e.to_string()),
        };
    }
    let (key, model) = (get("key"), get("model"));
    if !key.is_empty() && !gemini::looks_like_key(key) {
        return ("caution", "that is not a Gemini key", "A key is one line of letters, digits, - and _.".to_string());
    }
    if !model.is_empty()
        && let Err(why) = gemini::model_path(model)
    {
        return ("caution", "that is not a model name", why);
    }
    match gemini::save(key, model) {
        Err(e) => ("caution", "could not save", e.to_string()),
        Ok(saved) if !saved.has_key() => ("note", "saved", "Paste a key to turn Ask AI on.".to_string()),
        Ok(saved) => match gemini::check(&saved) {
            Ok(()) => ("help", "saved", format!("Gemini took the key, and knows {}.", saved.model())),
            Err(why) => ("caution", "saved, but Gemini did not take it", why),
        },
    }
}

// --- the posts --------------------------------------------------------------------------------------------------

/// Whether a path is one of the settings forms.
pub(crate) fn takes_post(path: &str) -> bool {
    path == "/ui/settings" || path.starts_with("/ui/settings/")
}

fn valid_group_name(group: &str) -> bool {
    !group.is_empty()
        && group.len() <= 512
        && group.contains('.')
        && group.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && !crate::filters::group_blocked(group)
}

fn apply_groups(cfg: &mut Config, args: &HashMap<String, String>, managed: &Managed) -> Result<String, String> {
    if managed.groups {
        return Err("Groups are managed outside Atlas.".into());
    }
    let group = args.get("group").map_or("", |value| value.trim());
    match args.get("do").map(String::as_str) {
        Some("add") => {
            if !valid_group_name(group) {
                return Err("Group names must be lowercase, contain a dot, and not be blocked.".into());
            }
            if cfg.groups.iter().any(|existing| existing == group) {
                return Err("That group is already configured.".into());
            }
            cfg.groups.push(group.to_string());
            cfg.group = group.to_string();
            Ok(format!("group {group} added"))
        }
        Some("remove") => {
            if cfg.groups.len() <= 1 {
                return Err("The last group stays.".into());
            }
            if !cfg.groups.iter().any(|existing| existing == group) {
                return Err("That group is not configured.".into());
            }
            cfg.groups.retain(|existing| existing != group);
            if cfg.group == group {
                cfg.group = cfg.groups.first().cloned().unwrap_or_default();
            }
            Ok(format!("group {group} removed"))
        }
        _ => Err("Choose add or remove.".into()),
    }
}

fn apply_keys(cfg: &mut Config, args: &HashMap<String, String>) -> Result<String, String> {
    let value = |name: &str| args.get(name).map_or("", |entry| entry.trim());
    let clear = |name: &str| args.get(&format!("clear_{name}")).is_some();
    let mut changed = 0;
    for (name, slot) in [
        ("tmdb_api_key", &mut cfg.media.tmdb.api_key),
        ("omdb_api_key", &mut cfg.media.omdb.api_key),
        ("tvdb_api_key", &mut cfg.media.tvdb.api_key),
        ("tvdb_pin", &mut cfg.media.tvdb.pin),
        ("trakt_client_id", &mut cfg.media.trakt.client_id),
        ("opensubtitles_api_key", &mut cfg.media.opensubtitles.api_key),
    ] {
        if clear(name) {
            if !slot.is_empty() {
                changed += 1;
            }
            slot.clear();
        } else if !value(name).is_empty() {
            *slot = value(name).to_string();
            changed += 1;
        }
    }
    Ok(if changed == 0 { "Nothing changed.".into() } else { format!("{changed} provider key(s) updated") })
}

/// A settings form, posted by the signed in browser. `csrf_header`: the `X-CSRF-Token` header, if sent.
pub(crate) fn post(path: &str, body: &str, csrf_header: Option<&str>, api_key: &str) -> Option<ApiResponse> {
    let args = parse_query(body);
    if !csrf_ok(&args, csrf_header, api_key) {
        let why = "The form was made for another sign in, or before atlas last started. It is not saved: try again.";
        return Some(html(403, settings_page(Some(("caution", "that form is stale", why.to_string())), api_key)));
    }
    let message = match path {
        "/ui/settings" => apply_gemini(&args),
        "/ui/settings/servers" => saved(|cfg, managed| apply_servers(cfg, &args, managed)),
        "/ui/settings/indexing" => saved(|cfg, managed| apply_indexing(cfg, &args, managed)),
        "/ui/settings/groups" => saved(|cfg, managed| apply_groups(cfg, &args, managed)),
        "/ui/settings/keys" => saved(|cfg, _| apply_keys(cfg, &args)),
        "/ui/settings/api" => {
            let mut moved = false;
            let message = saved(|cfg, managed| {
                apply_api(cfg, &args, managed).map(|(said, moves)| {
                    moved = moves;
                    said
                })
            });
            if moved && message.0 == "help" {
                restart_api_later();
            }
            message
        }
        _ => return None,
    };
    Some(html(200, settings_page(Some(message), api_key)))
}

/// Load, change, save: the terminal's own sequence, with the save's error said rather than the change kept.
fn saved(change: impl FnOnce(&mut Config, &Managed) -> Result<String, String>) -> Msg {
    let Some(mut cfg) = config::load_config() else {
        return ("caution", "not saved", "Configuration could not be read. Reload before saving.".into());
    };
    let managed = Managed::detect();
    match change(&mut cfg, &managed) {
        Err(why) => ("caution", "not saved", why),
        Ok(said) => match config::save_config(&cfg) {
            Ok(()) => ("help", "saved", said),
            Err(e) => ("caution", "could not write config.toml", e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "settings-key";

    fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn two_servers() -> Config {
        Config {
            servers: vec![
                UsenetServer { priority: 1, connections: Some(8), ..UsenetServer::new("news.a", "me", "pw-a", 563) },
                UsenetServer { priority: 2, ..UsenetServer::new("news.b", "me", "pw-b", 119) },
            ],
            ..Config::default()
        }
    }

    #[test]
    fn groups_can_be_added_and_removed_without_losing_current_group() {
        let mut cfg = Config {
            group: "alt.binaries.one".into(),
            groups: vec!["alt.binaries.one".into(), "alt.binaries.two".into()],
            ..Config::default()
        };
        let managed = Managed::default();
        assert!(apply_groups(&mut cfg, &form(&[("do", "add"), ("group", "alt.binaries.three")]), &managed).is_ok());
        assert_eq!(cfg.group, "alt.binaries.three");
        assert!(apply_groups(&mut cfg, &form(&[("do", "remove"), ("group", "alt.binaries.three")]), &managed).is_ok());
        assert_eq!(cfg.group, "alt.binaries.one");
        assert!(apply_groups(&mut cfg, &form(&[("do", "remove"), ("group", "alt.binaries.one")]), &managed).is_ok());
        assert_eq!(cfg.group, "alt.binaries.two");
    }

    #[test]
    fn provider_keys_are_write_only_and_empty_keeps_saved_value() {
        let mut cfg = Config::default();
        cfg.media.tmdb.api_key = "old".into();
        assert!(apply_keys(&mut cfg, &form(&[("tmdb_api_key", "new")])).is_ok());
        assert_eq!(cfg.media.tmdb.api_key, "new");
        assert!(apply_keys(&mut cfg, &form(&[])).is_ok());
        assert_eq!(cfg.media.tmdb.api_key, "new");
        assert!(apply_keys(&mut cfg, &form(&[("clear_tmdb_api_key", "on")])).is_ok());
        assert!(cfg.media.tmdb.api_key.is_empty());
    }

    fn server_fields<'a>(host: &'a str, port: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("host", host),
            ("username", "me"),
            ("port", port),
            ("connections", "10"),
            ("priority", "5"),
            ("ssl", "on"),
            ("index", "on"),
        ]
    }

    #[test]
    fn the_token_is_tied_to_the_session_and_not_the_key() {
        let token = csrf_token(KEY);
        assert_eq!(token.len(), 64);
        assert_ne!(token, csrf_token("another-key"));
        assert!(!token.contains(KEY) && token != session_token(KEY));
        assert!(csrf_ok(&form(&[(CSRF_FIELD, &token)]), None, KEY));
        assert!(csrf_ok(&form(&[]), Some(&token), KEY), "htmx sends it as a header");
        assert!(!csrf_ok(&form(&[]), None, KEY));
        assert!(!csrf_ok(&form(&[(CSRF_FIELD, &csrf_token("another-key"))]), None, KEY));
    }

    #[test]
    fn a_post_without_the_token_changes_nothing() {
        let resp = post("/ui/settings/indexing", "index_mode=live", None, KEY).unwrap();
        assert_eq!(resp.status, 403);
        assert!(resp.body.contains("that form is stale"), "{}", resp.body);
        assert!(post("/ui/settings/elsewhere", &format!("{CSRF_FIELD}={}", csrf_token(KEY)), None, KEY).is_none());
    }

    #[test]
    fn managed_settings_follow_the_environment_and_the_key_list() {
        assert_eq!(Managed::of(false, |_| false, &[]), Managed::default());
        let env = Managed::of(true, |name| name == "ATLAS_INDEX_MODE", &[]);
        assert!(env.servers && env.index_mode && env.api_host && !env.api_port && !env.groups);
        let keys: Vec<String> = ["api_key", "groups", "parallel_groups"].iter().map(|k| k.to_string()).collect();
        let listed = Managed::of(false, |_| false, &keys);
        assert!(listed.api_key && listed.groups && listed.parallel_groups && !listed.servers && !listed.index_mode);
    }

    #[test]
    fn a_server_is_added_with_the_terminals_rules() {
        let mut cfg = two_servers();
        let mut fields = server_fields("news.c", "563");
        fields.push(("do", "add"));
        fields.push(("password", "pw-c"));
        let said = apply_servers(&mut cfg, &form(&fields), &Managed::default()).unwrap();
        assert!(said.contains("news.c is added"), "{said}");
        assert_eq!(cfg.servers.len(), 3);
        let added = cfg.servers.iter().find(|s| s.host == "news.c").unwrap();
        assert_eq!(
            (added.password.as_str(), added.port, added.ssl, added.connections, added.priority),
            ("pw-c", 563, Some(true), Some(10), 5)
        );
        assert_eq!(added.index, None, "on is the default and stays out of the file");
        assert_eq!(cfg.servers.iter().map(|s| s.priority).collect::<Vec<_>>(), [1, 2, 5], "sorted by priority");

        // without a password, or with a bad port, nothing is taken
        for (change, why) in
            [(("password", ""), "password"), (("port", "0"), "port"), (("connections", "101"), "Connections")]
        {
            let mut cfg = two_servers();
            let mut fields = server_fields("news.d", "563");
            fields.push(("do", "add"));
            fields.push(("password", "pw-d"));
            fields.retain(|(k, _)| *k != change.0);
            fields.push(change);
            let err = apply_servers(&mut cfg, &form(&fields), &Managed::default()).unwrap_err();
            assert!(err.contains(why), "{err}");
            assert_eq!(cfg.servers.len(), 2);
        }
    }

    #[test]
    fn an_edit_keeps_the_password_when_the_field_is_empty_and_119_is_plain() {
        let mut cfg = two_servers();
        let mut fields = server_fields("news.b", "119");
        fields.extend([("do", "edit"), ("i", "1"), ("was", "news.b"), ("password", ""), ("index", "")]);
        fields.retain(|(k, _)| *k != "index");
        apply_servers(&mut cfg, &form(&fields), &Managed::default()).unwrap();
        let b = cfg.servers.iter().find(|s| s.host == "news.b").unwrap();
        assert_eq!(b.password, "pw-b");
        assert_eq!(b.ssl, Some(false), "ssl was ticked, but 119 is plain");
        assert_eq!(b.index, Some(false));

        // the page was stale: a row that moved isn't edited blind
        let mut fields = server_fields("news.z", "563");
        fields.extend([("do", "edit"), ("i", "1"), ("was", "news.a")]);
        let err = apply_servers(&mut cfg, &form(&fields), &Managed::default()).unwrap_err();
        assert!(err.contains("Reload"), "{err}");
    }

    #[test]
    fn the_last_server_and_a_duplicate_are_refused() {
        let mut cfg = two_servers();
        let remove = |i: &str, was: &str| form(&[("do", "remove"), ("i", i), ("was", was)]);
        assert!(
            apply_servers(&mut cfg, &remove("0", "news.a"), &Managed::default()).unwrap().contains("news.a is removed")
        );
        assert_eq!(cfg.servers.len(), 1);
        let err = apply_servers(&mut cfg, &remove("0", "news.b"), &Managed::default()).unwrap_err();
        assert!(err.contains("last server"), "{err}");

        let mut fields = server_fields("news.b", "119");
        fields.extend([("do", "add"), ("password", "x")]);
        let err = apply_servers(&mut cfg, &form(&fields), &Managed::default()).unwrap_err();
        assert!(err.contains("twice"), "{err}");

        let err = apply_servers(&mut cfg, &remove("0", "news.b"), &Managed { servers: true, ..Managed::default() })
            .unwrap_err();
        assert!(err.contains("ATLAS_NNTP_"), "{err}");
    }

    #[test]
    fn indexing_takes_a_known_mode_and_optional_numbers() {
        let mut cfg = Config::default();
        let said = apply_indexing(
            &mut cfg,
            &form(&[("index_mode", "live"), ("parallel_groups", "3"), ("max_unsaved_headers", "40000")]),
            &Managed::default(),
        )
        .unwrap();
        assert!(said.contains("mode live") && said.contains("3 groups at once"), "{said}");
        assert_eq!(
            (cfg.index_mode.as_str(), cfg.parallel_groups, cfg.max_unsaved_headers),
            ("live", Some(3), Some(40_000))
        );

        let err = apply_indexing(&mut cfg, &form(&[("index_mode", "fast")]), &Managed::default()).unwrap_err();
        assert!(err.contains("dynamic, live, backfill"), "{err}");
        assert_eq!(cfg.index_mode, "live", "nothing changed");
        let err =
            apply_indexing(&mut cfg, &form(&[("index_mode", "live"), ("parallel_groups", "0")]), &Managed::default())
                .unwrap_err();
        assert!(err.contains("Groups indexed at once"), "{err}");

        // empty goes back to worked out
        apply_indexing(
            &mut cfg,
            &form(&[("index_mode", "live"), ("parallel_groups", ""), ("max_unsaved_headers", "")]),
            &Managed::default(),
        )
        .unwrap();
        assert_eq!((cfg.parallel_groups, cfg.max_unsaved_headers), (None, None));

        // a managed mode is left alone, the rest taken
        let managed = Managed { index_mode: true, ..Managed::default() };
        apply_indexing(&mut cfg, &form(&[("index_mode", "backfill"), ("parallel_groups", "2")]), &managed).unwrap();
        assert_eq!((cfg.index_mode.as_str(), cfg.parallel_groups), ("live", Some(2)));
    }

    #[test]
    fn the_api_address_is_checked_and_says_when_it_moves() {
        let mut cfg = Config::default();
        let (said, moved) =
            apply_api(&mut cfg, &form(&[("api_host", "0.0.0.0"), ("api_port", "9191")]), &Managed::default()).unwrap();
        assert!(moved && said.contains("http://0.0.0.0:9191"), "{said}");
        let (_, moved) =
            apply_api(&mut cfg, &form(&[("api_host", "0.0.0.0"), ("api_port", "9191")]), &Managed::default()).unwrap();
        assert!(!moved);
        assert!(apply_api(&mut cfg, &form(&[("api_host", "a b"), ("api_port", "9191")]), &Managed::default()).is_err());
        assert!(apply_api(&mut cfg, &form(&[("api_host", "x"), ("api_port", "70000")]), &Managed::default()).is_err());
        assert_eq!(cfg.api_host, "0.0.0.0", "an error takes nothing, not even the half that was fine");
        let managed = Managed { api_host: true, api_port: true, ..Managed::default() };
        let (_, moved) = apply_api(&mut cfg, &form(&[("api_host", "::"), ("api_port", "1")]), &managed).unwrap();
        assert!(!moved && cfg.api_host == "0.0.0.0");
    }

    #[test]
    fn the_page_shows_every_server_without_its_password_and_marks_what_is_managed() {
        let cfg = Config { api_key: Some("the-api-key".into()), ..two_servers() };
        let html = render(&cfg, &Managed::default(), "tok", None);
        for secret in ["pw-a", "pw-b", "the-api-key"] {
            assert!(!html.contains(secret), "{secret} is on the page");
        }
        assert!(html.contains("news.a") && html.contains("news.b"));
        assert!(html.contains("password set") && html.contains("Add a server"), "{html}");
        assert!(html.contains("name=\"_csrf\" value=\"tok\"") && html.contains("X-CSRF-Token"), "{html}");
        assert!(html.contains("hx-boost=\"true\""));
        assert!(html.contains("<option value=\"dynamic\" selected>"), "{html}");

        let managed = Managed {
            servers: true,
            index_mode: true,
            api_host: true,
            api_port: true,
            api_key: true,
            ..Managed::default()
        };
        let html = render(&cfg, &managed, "tok", None);
        assert!(!html.contains("Add a server") && !html.contains("action=\"/ui/settings/servers\""), "{html}");
        assert!(
            html.contains("name=\"index_mode\" disabled") && html.contains("value=\"9090\" required disabled"),
            "{html}"
        );
        assert!(html.contains("ATLAS_NNTP_*") && html.contains("from outside atlas"), "{html}");
    }
}
