# Agent Rules (`AGENTS.md`)

## Persistent Web UI Footer & Attribution Rule

Every full HTML page served by the Web UI (`src/web.rs` via `page()`) **must** include a small persistent footer (`<footer class="site-footer">`) populated at compile time (`build.rs`) with:

- **Edition & Authors:** `Atlas - Rust Edition. Authored by: [Eraxty](https://github.com/Eraxty) & Forked and ported to Rust by: [xbmc4lyfe](https://github.com/xbmc4lyfe)`
- **Version & Commit:** `Version: <latest Rust git tag or v0.0.0.1 if untagged> · last git commit: [<7-char short SHA>](https://github.com/Appz4Fun/Atlas/commit/<full SHA>)`
- **Copyright & License:** `© <compile-time year> · License: [GPL v3 (GNU General Public License v3.0)](https://www.gnu.org/licenses/gpl-3.0.txt)`
- **Toolchain & Build Timestamp:** `Rust Version: <compiled rustc version> · Compilation Date: <UTC compilation timestamp>`
- **Patreon Support Link:** `Donate to my Patreon to help with AI token cost and get prioritized features and personal support:` with a badge/link to `https://www.patreon.com/cw/xbmc4lyfe`

Keep equivalent attribution, build/version info, and a prominent Patreon support callout (`https://www.patreon.com/cw/xbmc4lyfe`) in `README.md`.
