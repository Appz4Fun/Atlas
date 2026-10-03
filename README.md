<div align="center">

# Atlas

A self hosted Usenet indexer that lives in your terminal

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.99-orange?logo=rust)
![Platform](https://img.shields.io/badge/platform-Arch%20Linux%20(x86__64)-informational)
![Docker](https://img.shields.io/badge/docker-supported-2496ED?logo=docker&logoColor=white)
![Hackatime](https://hackatime.hackclub.com/api/v1/badge/U09JP15EVQU/Eraxty/Atlas)

[Features](#features) • [Install](#installation) • [Usage](#usage) • [Docker](#docker) • [Newznab API](#newznab-api-generic) • [Backup](#backing-up-the-database)

![Atlas](img/main.png)

</div>

---

## Why Atlas

I built Atlas because I wanted to make a Usenet indexer. A lot of indexers today are paid and expensive, Meanwhile atlas is opensource and free. Atlas keeps the useful parts in one place, it reads your provider, works out which posts belong together, stores them locally and makes an NZB or Download directly depending on your needs when you find something.

You can use it in the terminal. If you already run Prowlarr, SABnzbd, or the *arr apps, its Newznab API gets into that setup too.

## Features

- Index selected NNTP groups over SSL in live, backfill, or dynamic modes.
- Parse subjects into releases and mark incomplete sets.
- Search usenet through  local AI using Ollama.
- Watch indexing progress in the terminal.
- Generate NZBs and send downloads to the bundled SABnzbd.
- Keeps the data in a local SQLite database.
- Ships as a single native binary written in Rust (no Python needed to index, search or serve the API).

![Atlas dashboard](img/dash.png)

## Installation

You need:

- A Usenet provider account (NNTP, SSL enabled)
- [Rust](https://rustup.rs) 1.99 if you build from source. The exact version is pinned in `rust-toolchain.toml` and rustup installs it automatically; the `just` recipes use it even when another Rust (e.g. Homebrew) comes first on your PATH (`just toolchain` shows which one is used)
- Python 3 only if you want direct downloads through the bundled SABnzbd (SABnzbd itself is a Python app)

```bash
git clone https://github.com/Eraxty/Atlas
cd Atlas
cargo run --release
```

`cargo run` keeps `config.json`, `atlas.db` and the logs in the repo folder (set in `.cargo/config.toml`), the same place the old Python version kept them, so an existing setup carries straight over. To install it as a command instead:

```bash
cargo install --path .
ATLAS_HOME=~/.atlas atlas
```

An installed binary keeps its data next to itself unless `ATLAS_HOME` points somewhere else.

For direct downloads, SABnzbd's own Python dependencies need to be installed once:

```bash
pip install -r SABnzbd-5.0.4/requirements.txt
```

Prefer containers? Skip to [Docker](#docker).

### Binaries

It also got pre built binaries on the [release page](https://github.com/Eraxty/Atlas/releases).

#### Linux

1. Download `atlas-linux`.
2. Make it executable and run it:

```bash
chmod +x atlas-linux
./atlas-linux
```

#### Windows

1. Download `atlas-windows.zip`.
2. Extract it. It contains both `atlas-windows.exe` and `atlas.bat`.
3. Double click `atlas.bat`. It opens a terminal and starts Atlas.
4. 

If it doesnt work run it manually from a terminal

```powershell
cd atlas-windows
.\atlas-windows.exe
```

#### macOS

1. Download `atlas-macos`.
2. Make it executable and run it:

```bash
chmod +x atlas-macos
./atlas-macos
```

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

Your password is stored in your OS keyring when possible. If keyring isn't available it falls back to `config.json`.

### Multiple usenet servers

Atlas can use several providers. List them under `usenet_servers` in `config.json` (or add them from Settings -> Usenet servers):

```json
{
  "usenet_servers": [
    {"host": "news.primary.com",  "username": "me", "password": "...", "port": 563, "ssl": true, "connections": 10, "priority": 1},
    {"host": "news.backup.com",   "username": "me", "password": "...", "port": 563, "ssl": true, "priority": 2},
    {"host": "news.blockacct.nl", "username": "me", "password": "...", "port": 563, "ssl": true, "connections": 20, "priority": 3}
  ],
  "groups": ["alt.binaries.example"]
}
```

| Field | Meaning |
|---|---|
| `priority` | Lower is tried first. Servers with the same priority keep their order in the file. Missing means last (`99`) |
| `connections` | How many requests Atlas keeps in flight on that server at once (header fetches are split into parallel slices, par2/nfo lookups run side by side). Also what SABnzbd gets. Default `10` |
| `ssl` | Optional. Without it, port `563` means SSL and `119` means plain |
| `compress` | Ask the server for gzip compressed header listings (`XFEATURE COMPRESS GZIP`). Default `true`. Servers that don't support it just get plain requests, and one that sends something unreadable has it turned off automatically |
| `index` | Take part in indexing. Default `true`. `false` keeps the server for par2/nfo lookups only, e.g. a block account whose data you don't want spent on headers |

How the priority is used:

- Indexing uses every server at once, whatever its priority: groups are spread over the servers in proportion to their `connections`, and each group sticks to one server. Set `"index": false` on a server (e.g. a block account) to keep it out of indexing. A server that can't connect or log in is skipped for a minute and its groups move to the other servers. Priority decides which server is asked first for par2/nfo articles.
- A group the active server doesn't carry is looked up on the others.
- A par2/nfo article missing on one server is fetched from the next one, in priority order.
- Article numbers differ between providers, so indexing cursors are stored per server.
- Every server is added to SABnzbd with the same priority and connection count, so downloads fail over the same way.

Atlas also copes with a few provider quirks on its own:

- **Fill / bonus servers** that only serve articles (they answer `GROUP` with an error, e.g. `bonus.frugalusenet.com`) are detected automatically. They're used for par2/nfo lookups only and their groups go to the other servers. `"index": false` does the same up front.
- **Connection limits:** if a provider refuses another connection while Atlas already has some open to it (your plan's limit, or another app using the same account), Atlas lowers that server's connection count by one and waits for a free connection instead of failing requests. It logs this once per server.
- **Wrong username or password:** a rejected login takes that server out for 30 minutes with one clear log line, instead of retrying every minute. Fix the login in `config.json` (or Settings -> Usenet servers) and the running indexer picks it up within a few seconds.

#### Indexing speed

Each pass over a group takes `batch_size` article numbers, cut into `request_size` slices. The slices stream in over all of the server's connections, and each one is parsed and saved as soon as it arrives while the rest keep downloading. A connection picks up the next slice the moment it's free, so one slow reply doesn't hold the others up.

| Top level key | Default | Meaning |
|---|---|---|
| `batch_size` | `50000` | Article numbers per pass over a group. Larger means fewer pauses between passes, but the cursor only moves once the whole pass is done |
| `request_size` | `1000` | Article numbers per header request (one connection's slice) |
| `parallel_groups` | connections ÷ 5 per server | Total groups indexed at the same time, shared out between the servers by their `connections` (every server gets at least one). Unset means one group per 5 connections on each server, e.g. 4 servers × 25 connections gives 5 groups each, 20 in total |

Groups are indexed several at a time on every server at once, so all of the servers' connections stay busy (4 servers × 25 connections = 100 requests in flight). Each server never goes over its own `connections`. Groups are shared out by connection count, and each group sticks to one server because article numbers, and so the indexing cursors, differ between providers. If a group does move (its server is down, or you change servers or connection counts), its cursor on the new server starts from the top. That re-scans, but articles are de-duplicated, so nothing is stored twice.

The usual way to go faster is more `connections` on your indexing servers (check what each plan allows). The indexer picks up changes to these settings without a restart: groups, mode and batch settings apply within a few seconds, and changing servers or `parallel_groups` rebuilds the connection pool.

The old single server layout (`host`, `username`, `password`, `port` at the top level) still works. `atlas --selftest` checks the login on every server.

### Selecting groups

Go to Groups -> search -> add. Text groups and empty groups are filtered out by default.

### Indexing

> Note: indexing reads headers from the Usenet source server. It does not scan your computer.

Start the indexer from the main menu. Atlas starts pulling headers for every group you've selected. It runs as a background process (`atlas --bg-indexer`) so it keeps going after you close the menu. It also starts up the SABnzbd in the background so downloads are ready when you want them.You can also pick a mode for indexing bassed on your needs:-

| Mode | Behavior |
|---|---|
| `dynamic` | Alternates backfill and live passes, keeping up with new posts while building history |
| `backfill` | Indexes backward from the latest release only |
| `live` | Indexes forward from the latest release only and ignores older posts |

### Searching

Two search options are available :-

- Current group: search only the group you are in
- All groups: search everything you have indexed

AI search lets you describe what you want in plain language like (`find me 4k hdr movies`). The dumb AI selects the groups and keywords, then fetches anything missing from your database. It needs [Ollama](https://ollama.com) running locally, `qwen3:4b` model, Also speed of AI depends on your hardware because its local.

### Downloading

Select a release and chooseif u want to download NZB or download directly. Downloading directly will:

1. Start SABnzbd if it isn't already running
2. Generate an NZB for the selected release
3. Drop it into SABnzbd's watched folder
4. Opens SABnzbd in browser

Finished files are in `~/Downloads/complete`.

### Settings

- Change config: edit server credentials or groups without a full reset
- Change indexer mode: switch between same three modes told earlier
- Purge broken releases: delete incomplete releases to free up space
- Wipe DB and cache: clear the database, logs, status, and stats (stop the indexer first)
- Change API port: move the Newznab API off `9090` if that port is taken

## Docker

### Prebuilt image

No need to build locally just pull and run :-

```bash
docker pull ghcr.io/eraxty/atlas:latest
```

```bash
docker run -dit --name atlas \
  -e ATLAS_NNTP_HOST=news.your-provider.net \
  -e ATLAS_NNTP_USER=youruser \
  -e ATLAS_NNTP_PASS=yourpass \
  -e ATLAS_API_HOST=0.0.0.0 \
  -p 9090:9090 \
  -v atlas-data:/app/data \
  ghcr.io/eraxty/atlas:latest
```

needs `-it` because Atlas is a terminal app, and `ATLAS_API_HOST=0.0.0.0` lets the API be reached from outside the container.

Check if it's running :-

```bash
curl http://localhost:9090/api?t=caps
```

### Fresh Setup (compose stack)

The compose file runs Atlas, SABnzbd, and Prowlarr on same Docker network. They use service names instead of host IPs. Atlas reaches SABnzbd at `sabnzbd:8080` because they share the SABnzbd config volume.

1. Fill in your provider credentials in `docker/docker-compose.yml` (or in `docker/config.json`, see below):
   - `ATLAS_NNTP_HOST`, `ATLAS_NNTP_USER`, `ATLAS_NNTP_PASS`

   That file is tracked by git, so don't commit your real password into it.

2. Bring it up:

   ```bash
   docker compose -f docker/docker-compose.yml up -d
   ```

3. Grab the Atlas API key (generated on first run, then stored in the `atlas-data` volume so it survives restarts, and also written to config.json):

   ```bash
   docker compose -f docker/docker-compose.yml logs atlas | grep "api key"
   ```

4. Point Prowlarr at Atlas:
   - Open Prowlarr at `http://localhost:9696`
   - Go to Indexers then Add Indexer then Newznab
   - Name: `Atlas`
   - URL: `http://atlas:9090` (service name, same network)
   - API Path: `/api`
   - API Key: the key from step 3
   - Category: `Other` (7000)
   - Run Test. It should be green, then save the indexer

Atlas is exposed on `http://localhost:9090` for Prowlarr on the host setups too.

#### Verifying the stack

Run these two checks:

```bash
docker compose -f docker/docker-compose.yml ps
curl http://localhost:9090/api?t=caps
```

The first should show Atlas, Sabnzbd, and Prowlarr as `Up`. The second should return `<caps>` XML. This endpoint does not need an API key.

Atlas opens its setup wizard when the NNTP credentials are empty. Fill in `ATLAS_NNTP_USER` and `ATLAS_NNTP_PASS` in the compose file, then run
```bash
docker compose -f docker/docker-compose.yml up -d --force-recreate atlas
```

Stop everything with:

```bash
docker compose -f docker/docker-compose.yml down
```

Rebuild after code changes with:

```bash
docker compose -f docker/docker-compose.yml up -d --build
```

### Existing arr stack (just add the indexer)

If you are already running SABnzbd and Prowlarr, You don't need the full stack, run Atlas and point it at your existing services.

1. Run only Atlas (swap `docker build` + `atlas` for `docker pull ghcr.io/eraxty/atlas:latest` if you don't want to build):

   ```bash
   docker build -f docker/Dockerfile -t atlas .
   docker run -d --name atlas \
     -e ATLAS_NNTP_HOST=news.your-provider.net \
     -e ATLAS_NNTP_USER=youruser \
     -e ATLAS_NNTP_PASS=yourpass \
     -e ATLAS_SAB_HOST=172.17.0.1 \
     -e ATLAS_SAB_PORT=8080 \
     -e ATLAS_API_HOST=0.0.0.0 \
     -p 9090:9090 \
     -v atlas-data:/app/data \
     atlas
   ```

   `ATLAS_SAB_HOST` points to your SABnzbd. Use `172.17.0.1` when it runs on the host, host.docker.internal on Docker Desktop or the service name of its container. `ATLAS_API_HOST=0.0.0.0` is required so the published port can reach the API without it, Atlas defaults to `127.0.0.1` and stays on localhost.

2. Get the API key from the logs:

   ```bash
   docker logs atlas | grep "api key"
   ```

3. Add Atlas to your existing Prowlarr as a Newznab indexer:
   - Name: `Atlas`
   - URL: `http://<host-or-ip>:9090`
   - API Path: `/api`
   - API Key: the key from step 2
   - Category: `Other` (7000)
   - Run Test, then save

### Docker files

Everything Docker lives in `docker/`. The build context is the repo root, so build from there with `-f docker/Dockerfile` (the compose file already does).

| File | Purpose |
|---|---|
| `docker/Dockerfile` | Builds the Rust binary into a slim Debian image |
| `docker/docker-compose.yml` | Atlas + SABnzbd + Prowlarr, data in bind mounted folders next to it |
| `docker/config.example.json` | Template with your servers and no secrets. Copy it to `docker/config.json` and fill in the logins |
| `docker/config.json` | Git ignored. Mounted read only and copied into `docker/atlas-data/config.json` on the first start |
| `docker/docker-entrypoint.sh` | Does that first start copy, applies `PUID`/`PGID`/`UMASK`, then runs atlas |

After the first start Atlas keeps its own copy in `docker/atlas-data/` (groups you add, the API key and settings get saved there), so later edits to `docker/config.json` aren't picked up. To re-seed, stop the stack and delete `docker/atlas-data/config.json`.

If you start the stack before creating `docker/config.json`, Docker creates an empty folder with that name in its place. The container warns about it on startup; delete that folder and copy `config.example.json` as below.

#### File ownership (PUID / PGID)

The Atlas container reads `PUID`, `PGID` and `UMASK` the same way the linuxserver.io SABnzbd and Prowlarr images do. When `PUID`/`PGID` are set, it gives that user ownership of the data folder (`/app/data`) at startup and runs Atlas as that user. So the files it writes into `docker/atlas-data/`, and its edits to the `sabnzbd.ini` shared with the SABnzbd container, belong to the same user on the host. Use the same values as the SABnzbd service (the compose file uses `1000`).

| Variable | Default | Meaning |
|---|---|---|
| `PUID` | unset (root) | User id Atlas runs as. `0` or unset keeps root, like before |
| `PGID` | same as `PUID` | Group id |
| `UMASK` | image default | e.g. `022`, applied before Atlas starts |

On macOS (Docker Desktop / OrbStack) bind mounts always show your own user, so this mostly matters on Linux hosts.

```bash
cp docker/config.example.json docker/config.json   # then add usernames/passwords
docker compose -f docker/docker-compose.yml up -d --build
docker compose -f docker/docker-compose.yml run --rm atlas --selftest
```

### Environment variables

Set these in `docker/docker-compose.yml` (fresh setup) or on `docker run` (existing stack):

| Variable | Description |
|---|---|
| `ATLAS_NNTP_HOST` | Your provider's server |
| `ATLAS_NNTP_PORT` | Default `563` |
| `ATLAS_NNTP_USER` | Your username |
| `ATLAS_NNTP_PASS` | Your password |
| `ATLAS_NNTP_CONNECTIONS` | Requests in flight on that server. Default `10` |
| `ATLAS_INDEX_MODE` | `dynamic` / `live` / `backfill` |
| `ATLAS_API_PORT` | Port for Atlas's Newznab API Defaults to `9090` |
| `ATLAS_API_HOST` | Interface the API binds to. Defaults to `127.0.0.1` (localhost only). Set to `0.0.0.0` to accept connections from other hosts or containers. The compose stack sets this so Prowlarr can reach Atlas over the Docker network |
| `ATLAS_SAB_HOST` | Hostname of your SABnzbd (`sabnzbd` in the compose stack) |
| `ATLAS_SAB_PORT` | SABnzbd's port. Defaults to `8080` |
| `ATLAS_HOME` | Directory holding `config.json`, `atlas.db`, and logs the Docker image sets this to `/app/data` |
| `ATLAS_SAB_DIR` | Where the bundled SABnzbd lives. Defaults to `SABnzbd-5.0.4` next to the data dir, the binary or the current dir |
| `ATLAS_PYTHON` | Python interpreter used to launch SABnzbd. Defaults to `python3` / `python` on `PATH` |
| `OLLAMA_HOST` | Ollama server for AI search. Defaults to `127.0.0.1:11434` |
| `ATLAS_AI_MODEL` | Ollama model for AI search. Defaults to `qwen3:4b` |
| `ATLAS_NO_KEYRING` | Set to anything to skip the OS keyring and keep the password in `config.json` |
| `PUID` / `PGID` | Docker only. User / group id Atlas runs as, see [File ownership](#file-ownership-puid--pgid) |
| `UMASK` | Docker only. File creation mask applied before Atlas starts |

When the `ATLAS_NNTP_*` variables are set that server is tried first, ahead of any `usenet_servers` in `config.json`, but the groups you pick and the API key are still saved in `config.json`, so they survive container restarts.

## Newznab API (Generic)

Atlas exposes a generic Newznab compatible API, the protocol used by Prowlarr, Sonarr, Radarr, and SABnzbd. Any compatible client can search releases and fetch NZB's.

- Endpoint: `http://<host>:<port>/api`. The port defaults to `9090`.
- Binding: the API listens on `127.0.0.1` by default. Set `ATLAS_API_HOST=0.0.0.0` to expose it to other machines or containers.
- Authentication: `t=caps` works without a key. Every other operation needs the `apikey` parameter. A missing or wrong key returns a `401` Newznab error.

### Operations

| `t=` | Meaning | Auth |
|---|---|---|
| `caps` | Capability discovery (server info, supported params, categories) | No |
| `search` | Release search. `q` is optional; an empty query returns recent releases | Yes |
| `get` | Download the NZB for a release by `id` | Yes |

### Parameters

| Param | Applies to | Description |
|---|---|---|
| `apikey` | all (except `caps`) | Your API key |
| `q` | `search` | Plain word search terms matched against release names |
| `cat` | `search` | Accepted for compatibility; Atlas currently indexes category `7000` (Other) only |
| `limit` | `search` | Max results, default `100`, clamped to `100` |
| `offset` | `search` | Result offset for pagination |
| `id` | `get` | Release ID from a search result's `<guid>` |

### Examples

```bash
# capabilities (no auth)
curl "http://localhost:9090/api?t=caps"

# search releases, key required
KEY=$(jq -r .api_key config.json)
curl "http://localhost:9090/api?t=search&apikey=$KEY&q=matrix&limit=25"

# download the NZB for a specific release
curl -O "http://localhost:9090/api?t=get&id=1234&apikey=$KEY"
```

Under Docker, `config.json` lives in the `atlas-data` volume, so read the key from there instead of the host:

```bash
KEY=$(docker compose -f docker/docker-compose.yml exec -T atlas jq -r .api_key /app/data/config.json)
```

### Capabilities

`t=caps` advertises search (`q`, `limit`, `offset`, with up to 100 results), one category (`7000`, Other), and no registration. That is why Prowlarr should use category `Other (7000)` during setup.

### Search results

Each `<item>` carries Newznab-compatible metadata:

- `<title>`: release name
- `<guid>`: release ID, used with `t=get`
- `<link>` / `<enclosure>`: NZB download URL
- `<size>`: total size in bytes
- `<pubDate>`: posted date in RFC 2822 format
- `<newznab:attr name="category" value="7000"/>`: category

Prowlarr reads these to evaluate hits and hands the `<enclosure>` URL (Atlas's `t=get` endpoint) to the downloader. `t=get` responds with `application/x-nzb` and a `Content-Disposition` attachment header containing a valid NZB 1.1 file built from the indexed articles, so SABnzbd can grab it straight off the URL.

## Backing up the database

Under Docker, the database lives in a volume. Make sure the compose service is running then :-

```bash
docker compose -f docker/docker-compose.yml exec atlas cp /app/data/atlas.db /app/atlas.db
docker cp atlas:/app/atlas.db ./backup.db
```

Running from source :-

```bash
cp atlas.db ./backup.db
```

The database format is unchanged from the Python version, so old backups open fine.

## Platform

Tested on Arch Linux, x86_64, and macOS. Other platforms may work but aren't officially verified.

## Development

Common tasks are [just](https://github.com/casey/just) recipes (`just` lists them):

| Recipe | What it does |
|---|---|
| `just run` | Run Atlas optimized. Arguments pass through, e.g. `just run --selftest` |
| `just test` | Unit tests, a parity test against the old python subject parser, and end to end runs against mock NNTP servers |
| `just format` | `cargo fmt` (`just format-check` and `just lint` are what CI runs) |
| `just build` / `just build-release` | Optimized build, `target/release/atlas` |
| `just build-debug` | Debug build, `target/debug/atlas` |
| `just build-remote` / `just build-remote-release` | Optimized build on another machine over ssh, copied back to `target/remote/release/atlas` |
| `just build-remote-debug` | Same, debug build, `target/remote/debug/atlas` |

The remote recipes need `ATLAS_REMOTE_HOST` (any ssh destination with Rust installed, e.g. `ATLAS_REMOTE_HOST=me@linuxbox just build-remote`). The source is synced with rsync into `~/atlas-build` there (`ATLAS_REMOTE_DIR` to change it). Local data and secrets (`config.json`, `atlas.db`, logs) are never sent. Handy for getting a Linux binary from a Mac.

`atlas --selftest` checks the login on every configured server and exits.

## Limitation

Partial deobfuscation support via par2
Windows exe and Mac build might not work properly

## Credits

Built by [Me](https://github.com/Eraxty) over 60 days and 100+ hours. It is my biggest project so far Thanks to the Hack Club community for pushing me to build something like this.

AI helped with bug fixes, refactoring, SABnzbd integration, the background indexer, and terminal UI polish, Docker setup ,Testing and few more small things.

## License

[GPL-3.0](LICENSE). Bundled SABnzbd is GPL-2.0 or later and remains under its own license.
