//! The processing steps that name a release.

use rusqlite::{Connection, OptionalExtension};

use crate::archive::steps::{fetch_one, first_article};
use crate::archive::{self, Password};
use crate::config::Config;
use crate::nfo;
use crate::postprocess::step::{
    NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, mask,
};

use super::decide::{current_name, decide, effective_source, improves, worth_running};
use super::judge::{Grade, judge};
use super::{NAMING, NFO, NameSource};

pub struct Naming;

impl Step for Naming {
    fn id(&self) -> StepId {
        NAMING
    }
    fn name(&self) -> &'static str {
        "naming"
    }
    fn rank(&self) -> u16 {
        200
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.naming.enabled
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        worth_running(r)
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let inner = archive::store::inner_files(ctx.conn(), id).map_err(retry)?;
        // new inner files reach PreDB matching too, renamed or not. Bit 16 is its step: unregistered bits are ignored
        let rerun = if inner.is_empty() { 0 } else { mask(StepId(16)) };
        // an inline name is left alone until the user opts in with --reprocess
        let r = ctx.release();
        if r.display_name.is_some() && r.name_source.is_none() && !rename_named_is_on(ctx.conn()).map_err(retry)? {
            return Ok(Outcome { rerun, ..Outcome::default() });
        }
        let files = ctx.files().map_err(retry)?;
        let passworded = archive::store::password(ctx.conn(), id).map_err(retry)? == Some(Password::Yes);
        let name = decide(ctx.release(), &files, &inner, passworded);
        Ok(Outcome { matched: name.is_some(), name, rerun, ..Outcome::default() })
    }
}

/// A row means the user opted in, by `atlas --reprocess --step naming --apply`.
fn rename_named_is_on(conn: &Connection) -> rusqlite::Result<bool> {
    let row: Option<i64> = conn
        .prepare_cached("select value from meta where key = 'naming_rename_named'")?
        .query_row([], |r| r.get(0))
        .optional()?;
    Ok(row.is_some())
}

pub struct NfoName;

impl Step for NfoName {
    fn id(&self) -> StepId {
        NFO
    }
    fn name(&self) -> &'static str {
        "nfo"
    }
    fn rank(&self) -> u16 {
        210
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    // not tied to naming.enabled: with a budget the save path no longer reads NFOs, soo this is the only reader
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.archive.enabled && cfg.naming.nfo_step
    }
    fn applies(&self, r: &ReleaseInfo) -> bool {
        effective_source(r) <= NameSource::Nfo as i64 && judge(current_name(r)) < Grade::Good
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?;
        for file in archive::select::nfo_sources(&files) {
            let Some(seg) = first_article(ctx, file)? else { continue };
            let Some(body) = fetch_one(ctx, &seg, &file.filename)? else { continue };
            let found = nfo::name_candidates(&body).into_iter().find(|n| improves(ctx.release(), n, NameSource::Nfo));
            if let Some(name) = found {
                let change =
                    NameChange { display_name: Some(name), obfuscated: Some(false), source: NameSource::Nfo.value() };
                return Ok(Outcome { matched: true, name: Some(change), ..Outcome::default() });
            }
        }
        Ok(Outcome::default())
    }
}
