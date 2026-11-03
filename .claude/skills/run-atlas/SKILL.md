---
name: run-atlas
description: Build, run, start, drive, test or screenshot Atlas (the Rust Usenet indexer / Newznab server) locally against a mock Usenet server. Use to see a change in the real running app: index posts, search in the terminal menu, call the Newznab API, open or screenshot the web UI (release pages, dark/mobile), download an NZB, or run one test file.
---

# Run Atlas

Atlas is one Rust binary: an interactive terminal menu that starts the
Newznab API + web UI (same port) and spawns a headless `atlas --bg-indexer`
that indexes Usenet and runs post-processing. An agent drives it with
`.claude/skills/run-atlas/driver.sh`, which starts a **mock NNTP server**
(`mock_nntp.py`, 13 realistic posts: a movie with RAR/NFO/PAR2, a TV episode,
an album, an obfuscated upload) so no Usenet account is needed, runs the menu
in tmux, and screenshots the web UI with Playwright.

All paths are relative to the repo root. Nothing here touches prod
(`~/.atlas`): the driver uses `ATLAS_HOME=/tmp/atlas-run`.

## Prerequisites

The pinned toolchain (`rust-toolchain.toml`, 1.99.0) installs itself on the
first `cargo` call via rustup. `tmux`, `python3`, `curl`, `node` and
Playwright's Chromium were already in the container; no apt packages needed.

## Build

```bash
cargo build --release --locked      # ~4.5 min cold on this container -> target/release/atlas
```

## Run (agent path)

```bash
D=.claude/skills/run-atlas/driver.sh
$D up                    # mock NNTP :1119, menu in tmux "atlas", presses 1 (Start Indexing), waits for releases
                         # -> atlas up: web http://127.0.0.1:9099/  api key driver-key  home /tmp/atlas-run  releases 5
$D status                # menu / indexer / mock running?, release count
$D api 't=search&q=movie'          # Newznab API, apikey added for you
$D api 't=get&id=15' > x.nzb       # NZB for a release id (<guid> in search results)
$D shot /tmp/home                  # sign in + full-page screenshot of / -> /tmp/home.png
$D shot /tmp/rel '/ui/release?id=15'
$D shot /tmp/phone / --dark --width 390
$D shot /tmp/parts '/ui/release?id=15' --click 'details.meta-parts summary'   # open something first
$D screen                # print the terminal menu (tmux capture-pane, trailing blank rows dropped)
$D keys 2 Enter          # send keys: 2=Search, then 2=All Groups, then type a query
$D down                  # Exit menu, SIGTERM the indexer via bg_indexer.pid, stop the mock
rm -rf /tmp/atlas-run    # fresh start next time (up reuses an existing config.toml/db)
```

A menu search, start to finish (verified; one key batch per screen, with a
pause, because each screen reads its own line):

```bash
$D keys 2 Enter; sleep 1; $D keys 2 Enter; sleep 1; $D keys movie Enter; sleep 2; $D screen | tail -8
# ->  1  Cool.Movie.2024.1080p.WEB-DL.DDP5.1.H.264-GRP   2.9 MB  Movies/HD  6  2026-11-02
$D keys 0 Enter          # Back from results goes to the MAIN menu; one more 0 exits Atlas (and the API)
```

`shot` prints JSON with the page title and the first result names, so you can
check a page without opening the PNG. Then **look at the PNG** (Read it).

What you get after `up` (verified): 5 releases. Categories Movies/HD 2040,
Movies/UHD 2045, TV/HD 5040, Audio 3010; the obfuscated post is renamed
`Hidden.Name.2022.2160p.UHD.BluRay.x265-HDR` from its PAR2; the movie's
release page shows NFO, yEnc header, posting tool (Nyuu) and file list,
because post-processing fetched bodies from the mock. The album shows up as
two INCOMPLETE releases (the known track-file grouping limitation in the
README), which is what the code does today, not a driver bug.

Release ids are not 1..n (they step by 8 per shard): get them from
`$D api 't=search'` (`<guid>`), not by guessing. `15` was the movie in our run.

Want different data? Edit `build_posts()` in `mock_nntp.py`, then
`$D down; rm -rf /tmp/atlas-run; $D up`.

### One-shot CLI commands against the same data

```bash
ATLAS_HOME=/tmp/atlas-run ATLAS_NO_KEYRING=1 target/release/atlas --reprocess   # dry run
ATLAS_HOME=/tmp/atlas-run target/release/atlas --purge                          # dry run
```

## Test (internals: most PRs touch these)

```bash
cargo test --locked --test web_ui --test e2e   # one or two files; each tests/*.rs is its own binary (~1.5 min cold)
cargo test --locked                            # everything (what CI runs, with fmt + clippy)
cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings
```

The integration tests use their own Rust mock NNTP (`tests/common/mod.rs`) and a
temp `ATLAS_HOME`; `mock_nntp.py` is a stdlib port of it for the live app.

## Run (human path)

`just run` (= `cargo run --release`) opens the menu with data in the repo
root (`.cargo/config.toml` sets `ATLAS_HOME=.`); on first run it asks for a
server. Useless headless without tmux; use the driver.

## Gotchas

- **The API/web UI only exists while the interactive menu runs.** `--bg-indexer`
  alone indexes but serves nothing (`api::start` is called from `app::main_menu`).
  Hence tmux.
- **Exiting the menu (0) does not stop indexing**: the indexer is a detached
  process by design. Stop it with `kill $(cat $ATLAS_HOME/bg_indexer.pid)`
  (`down` does this). Exit is `0`, not `q`.
- **`pkill -f mock_nntp.py` (or any `ps | awk '/pattern/' | kill`) kills
  your own shell** when the command you ran contains that pattern (exit 144).
  The driver uses pid files instead.
- **Playwright from ESM fails** (`import 'playwright'` ignores `NODE_PATH`,
  where the container's copy lives), so `shot.cjs` is CommonJS.
- **Count releases with `grep -o '<item>' | wc -l`**: the RSS reply is one
  line, so `grep -c` says 1. And guard the grep (`|| true`): with no items
  yet it exits 1, and under `set -o pipefail` + `set -e` that silently ended
  `up` before it printed anything.
- **Playwright needs `PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers`** here. A
  stripped shell (`env -i`, cron) loses it and Playwright asks you to
  `npx playwright install` (don't). `shot.cjs` sets it when missing.
- `up` waits until the release count holds still for 3s: post-processing
  lands releases a few at a time, and returning at the first one gave 4 of 5.
- `ATLAS_NO_KEYRING=1` keeps the mock password in `config.toml` instead of
  trying the Linux secret service (no D-Bus here).
- The driver's config sets `settle_secs = 0` / `complete_settle_secs = 0` so
  post-processing runs at once; the mock's posts are dated 4h back too.
- **`0` from search results is Back to the main menu, not to the Search
  submenu**, so a habitual second `0` exits Atlas and the web UI/API with it
  (the indexer keeps running). `status` then says `menu: exited`; `up` again
  restarts the menu on the same data in ~4s.
- **`$(tmux capture-pane) | tail` shows only blank lines**: the pane is 50 rows
  and every menu draws at the top. `screen` strips the trailing blank rows.
- tmux window is 160x50: the ratatui panels wrap badly under ~120 columns.

## Troubleshooting

- `no target/release/atlas` from `up`: build first.
- `api never came up` (exit 1, menu screen printed): something else holds the
  API port. Use another: `API_PORT=9199 $D up` (on a fresh `ATLAS_HOME`,
  since the port is written into `config.toml` on first `up`).
- Few or no releases: read `/tmp/atlas-run/bg_index.log` and
  `/tmp/atlas-run/mock_nntp.log`. A mock already listening on 1119 is reused
  as-is, so after editing `mock_nntp.py` run `down` first.
