<div align="center">

# Atlas

A self hosted Usenet indexer and Newznab server that lives in your terminal

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.99-orange?logo=rust)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-informational)
![Docker](https://img.shields.io/badge/docker-supported-2496ED?logo=docker&logoColor=white)
![Hackatime](https://hackatime.hackclub.com/api/v1/badge/U09JP15EVQU/Eraxty/Atlas)

[Features](#features) • [Install](#installation) • [config.json](#configuration-configjson) • [Usage](#usage) • [Newznab API](#newznab-api) • [Docker](#docker) • [Development](#development)

![Atlas](img/main.png)

</div>

---

## Why Atlas

I built Atlas because I wanted to make a Usenet indexer. A lot of indexers today are paid and expensive, Meanwhile atlas is opensource and free. Atlas keeps the useful parts in one place, it reads your provider, works out which posts belong together, stores them locally and makes an NZB when you find something.

You can use it in the terminal, or point Prowlarr, NZBHydra2, Sonarr, Radarr or any other Newznab client at it and grab NZBs straight from its API.

Atlas is now a **single native binary written in Rust**. It replaces the original Python version: same menus, same `config.json` and same `atlas.db`, so an existing setup carries straight over, but it indexes many times faster.

## Features

- **Index usenet headers** from the groups you pick, over SSL, in live, backfill or dynamic modes.
- **Use all your usenet servers at once.** List as many providers as you like in `config.json`. Every server indexes in parallel, with each one's connections kept busy (4 servers × 25 connections = 100 requests in flight).
- **Fast:** header requests are split into slices that stream over all connections, each slice is saved the moment it arrives, and many groups index at the same time. Compressed header listings (`XFEATURE COMPRESS GZIP`) are used when the server offers them.
- **Copes with real providers:** fill/bonus servers that only serve articles are detected automatically, Atlas lowers its own connection count when a provider refuses more, and wrong logins are set aside instead of hammered.
- **Releases, not posts:** subjects are parsed into releases, incomplete sets are marked, and obfuscated posts get their real name from the par2 or nfo files.
- **Newznab API** for Prowlarr / NZBHydra2 / Sonarr / Radarr: `search`, `tvsearch`, `movie`, `music`, `book`, `caps` and `get` (NZB download).
- **Search** in the terminal (one group or all), plus an **AI search** that turns "find me 4k hdr movies" into groups and keywords using a local [Ollama](https://ollama.com) model.
- **Live dashboard** with throughput, totals and per group progress.
- **Everything local** in a SQLite database with a full text index.
- **Docker** image and compose stack, with `PUID`/`PGID` support.

![Atlas dashboard](img/dash.png)

## Installation

You need:

- A Usenet provider account (NNTP, SSL enabled)
- [Rust](https://rustup.rs) to build it. The exact version (1.99) is pinned in `rust-toolchain.toml` and rustup installs it automatically
- Optionally [just](https://github.com/casey/just) for the shortcuts below

```bash
git clone https://github.com/Appz4Fun/Atlas
cd Atlas
just run            # or: cargo run --release
```

`just run` / `cargo run` keep `config.json`, `atlas.db` and the logs in the repo folder (set in `.cargo/config.toml`), the same place the old Python version kept them, so an existing setup carries straight over. The `just` recipes always build with the pinned toolchain, even when another Rust (e.g. Homebrew) comes first on your PATH (`just toolchain` shows which one is used).

To install it as a command instead:

```bash
cargo install --path .
ATLAS_HOME=~/.atlas atlas
```

An installed binary keeps its data next to itself unless `ATLAS_HOME` points somewhere else.

```text
usage: atlas [--selftest | --bg-indexer]

  (no args)     interactive menu
  --selftest    check the login on every usenet server and exit
  --bg-indexer  run the indexing loop headless (the menu starts this for you)
```

Prefer containers? Skip to [Docker](#docker).

### Binaries

Tagging a release (`v*`) builds `atlas-linux`, `atlas-macos` and `atlas-windows.zip` (exe + `atlas.bat` launcher) with the `build-executables` workflow and attaches them to the GitHub release.

- **Linux / macOS:** `chmod +x atlas-linux && ./atlas-linux`
- **Windows:** extract `atlas-windows.zip` and double click `atlas.bat`, or run `.\atlas-windows.exe` from a terminal

## Configuration (`config.json`)

On first run Atlas asks for one server (host, username, password, port) and writes `config.json`. Everything else is edited in the menus or directly in the file. A secret free template is in [`config.example.json`](config.example.json).

### Full example

```json
{
    "usenet_servers": [
        {"host": "news.provider-a.com", "username": "me", "password": "secret", "port": 563, "ssl": true, "connections": 50, "priority": 1},
        {"host": "news.provider-b.com", "username": "me", "password": "secret", "port": 563, "ssl": true, "connections": 30, "priority": 2},
        {"host": "fill.provider-c.com", "username": "me", "password": "secret", "port": 563, "connections": 20, "priority": 3, "index": false},
        {"host": "news.local-test",     "username": "me", "password": "secret", "port": 119, "ssl": false, "compress": false}
    ],
    "group": "alt.binaries.example",
    "groups": ["alt.binaries.example", "alt.binaries.another"],
    "index_mode": "dynamic",
    "batch_size": 50000,
    "request_size": 1000,
    "parallel_groups": 20,
    "api_host": "0.0.0.0",
    "api_port": 9090,
    "api_key": "generated-on-first-start"
}
```

### Top level keys

| Key | Default | Meaning |
|---|---|---|
| `usenet_servers` | | List of servers, see below |
| `groups` | `[]` | Newsgroups to index. Added from the Groups menu, or edit the list |
| `group` | | The "current group" used by the menu's current-group search |
| `index_mode` | `dynamic` | `dynamic` (backfill and live passes in turn), `backfill` (older posts only) or `live` (new posts only) |
| `batch_size` | `50000` | Article numbers per indexing pass over a group. The cursor only moves once a whole pass is done |
| `request_size` | `1000` | Article numbers per header request (one connection's slice of a pass) |
| `parallel_groups` | connections ÷ 5 per server | Groups indexed at the same time in total, shared out between servers by their `connections` (every server gets at least one). Unset means one group per 5 connections on each server |
| `api_host` | `127.0.0.1` | Address the Newznab API listens on. `0.0.0.0` makes it reachable from other machines and containers |
| `api_port` | `9090` | Newznab API port |
| `api_key` | generated | Newznab API key, made on first start and kept here |

Atlas keeps any other keys you add to the file when it saves it.

### Server fields (`usenet_servers`)

| Field | Default | Meaning |
|---|---|---|
| `host` | | Provider's NNTP server, domain only |
| `username` / `password` | | Login. With only one server in the old layout the password goes to the OS keyring when possible; in `usenet_servers` a password already written in the file stays there |
| `port` | `563` | `563` is SSL, `119` is plain |
| `ssl` | from the port | Force SSL on or off |
| `connections` | `10` | Requests Atlas keeps in flight on this server at once. Set it to what your plan allows, see [Measuring connection limits](#measuring-connection-limits) |
| `priority` | `99` | Lower is asked first when looking up par2/nfo articles. Equal priorities keep their order in the file |
| `index` | `true` | Take part in indexing. `false` keeps the server for article lookups only, e.g. a block account whose data you don't want spent on headers |
| `compress` | `true` | Ask for gzip compressed header listings. Servers without it just get plain requests; one that sends unreadable data has it turned off automatically |

The old single server layout (`host`, `username`, `password`, `port` at the top level) still works and becomes a one-server list.

### How the servers are used

- **Indexing uses every server at once, whatever its priority.** Groups are spread over the indexing servers in proportion to their `connections`, and each server runs its own groups, so all of their connections stay busy. Each group sticks to one server, because article numbers (and so the indexing cursors) differ between providers.
- **A group a server doesn't carry** is looked up on the others.
- **par2/nfo articles** are asked of the servers in priority order until one has them.
- **A server that can't connect** is skipped for a minute and its groups move to the other servers. If a group moves, its cursor on the new server starts from the top. That re-scans, but articles are de-duplicated, so nothing is stored twice.

Atlas also copes with a few provider quirks on its own:

- **Fill / bonus servers** that only serve articles (they answer `GROUP` with an error) are detected automatically and used for article lookups only. `"index": false` does the same up front.
- **Connection limits:** if a provider refuses another connection while Atlas already has some open to it (your plan's limit, or another app on the same account), Atlas lowers that server's connection count by one and waits for a free connection instead of failing requests. It logs this once per server.
- **Wrong username or password:** a rejected login takes that server out for 30 minutes with one clear log line, instead of retrying every minute.

### Changes apply live

The background indexer re-reads `config.json` every 5 seconds. New groups, `index_mode`, `batch_size` and `request_size` apply right away; changing servers, logins, `connections` or `parallel_groups` rebuilds the connection pool. Fixing a wrong login in the file (or in Settings → Usenet servers) is picked up within a few seconds. `api_host` / `api_port` apply when the API restarts (restart Atlas, or Settings → Change api port).

### Measuring connection limits

To find what each provider really allows, stop indexing and run:

```bash
ATLAS_HOME=. cargo run --release --example probe_connections -- --max 300
```

It opens and logs in connections on every server until the provider refuses one, then reports the maximum per server, and checks whether servers on the same account share a limit. Setting `connections` a bit below the measured maximum (e.g. 75%) leaves headroom for other apps on the same account.

## Usage

![Setup](img/login.png)

### First time setup

On first run, Atlas asks for your provider credentials:

| Field | What to enter |
|---|---|
| Host | Your provider's NNTP server. Use the domain only, e.g. `news.yourprovider.net` |
| Username | Your provider username |
| Password | Your provider password |
| Port | `563` for SSL. Leave the default as it is |

Leave the host empty to quit without writing anything (handy if your `config.json` is somewhere else: point `ATLAS_HOME` at its folder). The startup panel always shows which `config.json` was loaded. Add more servers afterwards in Settings → Usenet servers or in `config.json`.

### Selecting groups

Go to Groups, search (at least 3 letters, e.g. `movies`) and pick a number to add it. Only binary groups that aren't empty are listed. Remove groups from the main menu (Remove group).

### Indexing

> Note: indexing reads headers from your Usenet provider. It does not scan your computer.

Start the indexer from the main menu (1). It runs as a background process (`atlas --bg-indexer`), so it keeps going after you close the menu, and indexes many groups at once on all your servers. The menu shows its state, e.g. `3197 groups, 90 at once`, and `[WARNING]` with an error count when a server or group is having trouble (details are in `bg_index.log`).

| Mode | Behavior |
|---|---|
| `dynamic` | Alternates backfill and live passes, keeping up with new posts while building history |
| `backfill` | Indexes backward from the latest article only |
| `live` | Indexes forward from the latest article only and ignores older posts |

A group that is caught up rests for 10 seconds before it's checked again. A group that keeps failing (3 errors in a row) is parked for 5 minutes.

### Live dashboard

Menu option 5: status, totals, throughput (in MB/s or GB/s of indexed posts) and per group progress. `q`, `Esc` or `Ctrl+C` goes back.

### Searching

- Current group: search only the group you are in
- All groups: search everything you have indexed
- Obfuscated posts: releases that still only have a random name

Searches match the posted name and the real name found in par2/nfo files. Pick a result to see its files and save its NZB.

AI search lets you describe what you want in plain language (`find me 4k hdr movies`). The AI picks the groups and keywords, then fetches anything missing from your database. It needs [Ollama](https://ollama.com) running with the `qwen3:4b` model (or `ATLAS_AI_MODEL`); speed depends on your hardware.

### Settings

- Usenet servers: list, add, edit and remove servers (host, login, port, SSL, connections, priority)
- Change indexer mode: dynamic / live / backfill
- Purge broken releases: delete incomplete releases to free up space
- Wipe DB and cache: clear the database, logs, status and stats (stop the indexer first)
- Change API port: move the Newznab API, and restart it with the current `api_host`

## Newznab API

Atlas exposes a Newznab compatible API, the protocol Prowlarr, NZBHydra2, Sonarr and Radarr use. Any compatible client can search releases and fetch NZBs.

- Endpoint: `http://<host>:<port>/api`, port `9090` by default. Opening `http://<host>:<port>/` in a browser shows a short page with the settings to use.
- Binding: `127.0.0.1` by default. Set `"api_host": "0.0.0.0"` in `config.json` (or `ATLAS_API_HOST`) to reach it from other machines or containers, then restart Atlas.
- Authentication: `t=caps` works without a key. Every other call needs `apikey`; a wrong key returns a `401` Newznab error.

### Operations

| `t=` | Meaning | Auth |
|---|---|---|
| `caps` | Capabilities: server info, supported search types and params, categories | No |
| `search` | Release search. `q` is optional; an empty query returns recent releases | Yes |
| `tvsearch` | TV search. `season` and `ep` are added to the query as `S01E02` | Yes |
| `movie` | Movie search by `q` | Yes |
| `music` / `audio` | Music search by `q` | Yes |
| `book` | Book search by `q` | Yes |
| `get` | The NZB for a release, by `id` | Yes |

Atlas only knows release names, not TVDB/IMDb ids. A search with only an id (`tvdbid`, `imdbid`, `tmdbid`, `tvmazeid`, `rid`, `traktid`, ...) returns an empty result rather than an error, and an id next to `q` searches by `q`. Unknown `t=` values get a proper Newznab error (`203`) instead of a 404, so capability checks (e.g. NZBHydra2's) complete.

### Parameters

| Param | Applies to | Description |
|---|---|---|
| `apikey` | all but `caps` | Your API key |
| `q` | searches | Words matched against release names (posted and real names) |
| `season`, `ep` | `tvsearch` | Season / episode, matched as `S01E02` |
| `cat` | searches | Accepted for compatibility; everything is category `7000` |
| `limit` | searches | Max results, default `100`, clamped to `100` |
| `offset` | searches | Result offset for paging |
| `id` | `get` | Release id from a result's `<guid>` |

### Examples

```bash
# capabilities (no key)
curl "http://localhost:9090/api?t=caps"

# search, key required
KEY=$(jq -r .api_key config.json)
curl "http://localhost:9090/api?t=search&apikey=$KEY&q=matrix&limit=25"
curl "http://localhost:9090/api?t=tvsearch&apikey=$KEY&q=Some+Show&season=1&ep=2"

# download an NZB
curl -OJ "http://localhost:9090/api?t=get&id=1234&apikey=$KEY"
```

### Adding Atlas to Prowlarr / NZBHydra2

- Type: Newznab (generic)
- URL: `http://<atlas-host>:9090`
- API path: `/api`
- API key: `api_key` from `config.json`
- Category: `Other (7000)`

### Search results

Each `<item>` carries `<title>` (release name), `<guid>` (release id for `t=get`), `<link>` / `<enclosure>` (NZB URL), `<size>` (bytes), `<pubDate>` (RFC 2822) and `<newznab:attr name="category" value="7000"/>`. `t=get` answers with `application/x-nzb` and a `Content-Disposition` attachment holding a valid NZB 1.1 file built from the indexed articles, so a downloader can grab it straight off the URL.

## Docker

Everything Docker lives in `docker/`. The build context is the repo root, so build from there with `-f docker/Dockerfile` (the compose file already does). The image is a slim Debian with just the `atlas` binary.

| File | Purpose |
|---|---|
| `docker/Dockerfile` | Builds the Rust binary (pinned Rust version, Debian trixie) |
| `docker/docker-compose.yml` | Atlas + SABnzbd + Prowlarr, data in bind mounted folders next to it |
| `docker/config.example.json` | Template with servers and no secrets. Copy it to `docker/config.json` and fill in the logins |
| `docker/config.json` | Git ignored. Mounted read only and copied into `docker/atlas-data/config.json` on the first start |
| `docker/docker-entrypoint.sh` | Does that first start copy, applies `PUID`/`PGID`/`UMASK`, then runs atlas |

### Compose stack

```bash
cp docker/config.example.json docker/config.json   # then add usernames/passwords
docker compose -f docker/docker-compose.yml up -d --build
docker compose -f docker/docker-compose.yml run --rm atlas --selftest
docker compose -f docker/docker-compose.yml logs atlas | grep "api key"
```

Then add Atlas to Prowlarr (`http://localhost:9696`) as a Newznab indexer with URL `http://atlas:9090` (service name on the shared network), API path `/api`, the key from the logs, category `Other (7000)`.

After the first start Atlas keeps its own copy of the config in `docker/atlas-data/` (groups you add, the API key and settings are saved there), so later edits to `docker/config.json` aren't picked up. To re-seed, stop the stack and delete `docker/atlas-data/config.json`. If you start the stack before creating `docker/config.json`, Docker creates an empty folder with that name; the container warns about it on startup. Delete the folder and copy `config.example.json` as above.

Check it's up:

```bash
docker compose -f docker/docker-compose.yml ps
curl http://localhost:9090/api?t=caps
```

### Just Atlas

```bash
docker build -f docker/Dockerfile -t atlas .
docker run -dit --name atlas \
  -v "$PWD/docker/config.json:/app/config.json:ro" \
  -v atlas-data:/app/data \
  -e ATLAS_API_HOST=0.0.0.0 \
  -p 9090:9090 \
  atlas
```

It needs `-it` because Atlas is a terminal app. Instead of a config file you can pass one server with `ATLAS_NNTP_HOST`, `ATLAS_NNTP_USER`, `ATLAS_NNTP_PASS`.

### File ownership (PUID / PGID)

The container reads `PUID`, `PGID` and `UMASK` the same way the linuxserver.io images do. When set, it gives that user the data folder (`/app/data`) at startup and runs Atlas as that user, so files in `docker/atlas-data/` belong to the same user on the host. Use the same values as your other containers (the compose file uses `1000`).

| Variable | Default | Meaning |
|---|---|---|
| `PUID` | unset (root) | User id Atlas runs as. `0` or unset keeps root |
| `PGID` | same as `PUID` | Group id |
| `UMASK` | image default | e.g. `022`, applied before Atlas starts |

On macOS (Docker Desktop / OrbStack) bind mounts always show your own user, so this mostly matters on Linux hosts.

## Environment variables

| Variable | Description |
|---|---|
| `ATLAS_HOME` | Folder holding `config.json`, `atlas.db` and logs. `cargo run` uses the repo folder, an installed binary its own folder, the Docker image `/app/data` |
| `ATLAS_NNTP_HOST` / `_PORT` / `_USER` / `_PASS` | One server from the environment (Docker). It goes first, ahead of `usenet_servers`; groups and the API key are still saved in `config.json`, and these creds are never written to it |
| `ATLAS_NNTP_CONNECTIONS` | Connections for that server. Default `10` |
| `ATLAS_INDEX_MODE` | `dynamic` / `live` / `backfill` |
| `ATLAS_API_HOST` | Interface the API binds to, see `api_host` |
| `ATLAS_API_PORT` | API port, see `api_port` |
| `OLLAMA_HOST` | Ollama server for AI search. Default `127.0.0.1:11434` |
| `ATLAS_AI_MODEL` | Ollama model for AI search. Default `qwen3:4b` |
| `ATLAS_NO_KEYRING` | Skip the OS keyring and keep passwords in `config.json` |
| `ATLAS_SAB_HOST` / `ATLAS_SAB_PORT` / `ATLAS_SAB_DIR` / `ATLAS_PYTHON` | Optional SABnzbd hand off, see below |
| `PUID` / `PGID` / `UMASK` | Docker only, see [File ownership](#file-ownership-puid--pgid) |

### Direct downloads (SABnzbd, optional)

The menu's Download option can still hand a release to SABnzbd: Atlas writes the NZB into SABnzbd's watched folder and syncs your servers into its config. It uses the bundled `SABnzbd-5.0.4` (needs Python 3 and `pip install -r SABnzbd-5.0.4/requirements.txt`) or an existing SABnzbd at `ATLAS_SAB_HOST`:`ATLAS_SAB_PORT`. Atlas is primarily a Newznab server now; the usual setup is a downloader (SABnzbd, NZBGet, ...) fetching NZBs from Atlas's API through Prowlarr or your *arr apps.

## Backing up the database

```bash
cp atlas.db ./backup.db                       # running from source (stop indexing first)
docker compose -f docker/docker-compose.yml exec atlas cp /app/data/atlas.db /app/atlas.db
docker cp atlas:/app/atlas.db ./backup.db     # docker
```

The database format is the same as the Python version's, so old databases and backups open fine. On first start an older database gets its search index rebuilt once to include par2/nfo names (about 20 seconds on a 7 GB database).

## Development

Common tasks are [just](https://github.com/casey/just) recipes (`just` lists them):

| Recipe | What it does |
|---|---|
| `just run` | Run Atlas optimized. Arguments pass through, e.g. `just run --selftest` |
| `just test` | All tests: unit tests, a parity test against the old Python subject parser, and end to end runs against mock NNTP servers (indexing, search, NZB, API, failover, parallel groups, compression, provider quirks) |
| `just format` | `cargo fmt` (`just format-check` and `just lint` are what CI runs) |
| `just lint` | `cargo clippy` with warnings as errors |
| `just build` / `just build-release` | Optimized build, `target/release/atlas` |
| `just build-debug` | Debug build, `target/debug/atlas` |
| `just build-remote` / `just build-remote-release` | Optimized build on another machine over ssh, copied back to `target/remote/release/atlas` |
| `just build-remote-debug` | Same, debug build |
| `just toolchain` | Show which Rust the recipes use |

The remote recipes need `ATLAS_REMOTE_HOST` (any ssh destination with Rust installed, e.g. `ATLAS_REMOTE_HOST=me@linuxbox just build-remote`). The source is synced with rsync into `~/atlas-build` there (`ATLAS_REMOTE_DIR` to change it); `config.json`, `atlas.db` and logs are never sent. Per machine settings like these can go in a git ignored `.env` (see `.env.example`), which `just` loads.

CI (`.github/workflows/ci.yml`) runs formatting, clippy and the tests on Linux, macOS and Windows.

### Layout

| Path | What |
|---|---|
| `src/nntp.rs` | Async NNTP client and per server connection pools (tokio + rustls), compression, failover |
| `src/indexer.rs` | One indexing pass over a group: slices, parsing, naming, saving |
| `src/bg_indexer.rs` | The background scheduler: workers per server, live config reload, status/stats |
| `src/parser.rs`, `par2.rs`, `nfo.rs` | Subjects to releases, real names from par2/nfo |
| `src/db.rs`, `search.rs` | SQLite schema, migrations, full text search |
| `src/api.rs`, `nzb.rs` | Newznab API and NZB building |
| `src/app.rs`, `ui.rs`, `dashboard.rs`, `groups_menu.rs`, `ai.rs` | The terminal UI |
| `examples/probe_connections.rs` | Connection limit probe |

## Limitations

- Deobfuscation only works when a release has a par2 or nfo file with a usable name.
- Everything is reported as category `7000` (Other); clients that only search TV or movie categories may skip Atlas.
- Search matches release names only, so id based searches (TVDB, IMDb) return nothing.

## Credits

Built by [Me](https://github.com/Eraxty) over 60 days and 100+ hours. It is my biggest project so far Thanks to the Hack Club community for pushing me to build something like this.

AI helped with bug fixes, refactoring, SABnzbd integration, the background indexer, and terminal UI polish, Docker setup ,Testing and few more small things.

The Rust port (multi-server parallel indexing, the async NNTP client and the extended Newznab API) lives in the [Appz4Fun fork](https://github.com/Appz4Fun/Atlas).

## License

[GPL-3.0](LICENSE). Bundled SABnzbd is GPL-2.0 or later and remains under its own license.
