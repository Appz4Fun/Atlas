//! The processing steps that name a release.

use crate::archive::steps::{fetch_one, first_article};
use crate::archive::{self, Password};
use crate::config::Config;
use crate::nfo;
use crate::postprocess::step::{
    NameChange, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite, mask,
};
use crate::release_info;

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
        let id = ctx.release().id;
        let inner = ctx.data().archive_inner_files(id)?;
        // new inner files reach PreDB matching too, renamed or not. Bit 16 is its step: unregistered bits are ignored
        let rerun = if inner.is_empty() { 0 } else { mask(StepId(16)) };
        let r = ctx.release();
        let cleared = r.name_source == Some(NameSource::Cleared as i64);
        let inline = r.display_name.is_some() && r.name_source.is_none();
        let opted_in = (cleared || inline) && ctx.data().rename_named(&r.group)?;
        // an inline name stays as it is until the user opts in with --reprocess
        if inline && !opted_in {
            return Ok(Outcome { rerun, ..Outcome::default() });
        }
        // and a name the user cleared came from subject text, soo only what's inside the post can name it again
        let files = if cleared && !opted_in { Vec::new() } else { ctx.files().map_err(StepError::from_inventory)? };
        let passworded = ctx.data().archive_password(id)? == Some(Password::Yes);
        let name = decide(ctx.release(), &files, &inner, passworded);
        Ok(Outcome { matched: name.is_some(), name, rerun, ..Outcome::default() })
    }
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
    fn skip_reason(&self, r: &ReleaseInfo) -> Option<&'static str> {
        if effective_source(r) > NameSource::Nfo as i64 {
            Some("its name came from something surer than an NFO")
        } else if judge(current_name(r)) >= Grade::Good {
            Some("its name is already good: an NFO can't better it")
        } else {
            None
        }
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let rel_id = ctx.release().id;
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let content_id = ctx.data().release_content_id(rel_id)?.unwrap_or_default();
        let existing_db_pw = ctx.data().nfo_password(rel_id)?;
        let (names_hidden, probe_encrypted) = ctx.data().nfo_password_rerun_flags(rel_id)?;
        let mut writes = Vec::new();
        let mut rerun = 0;
        if let Some(saved_pw) = crate::raw_store::get_nfo_password_checked(&rel_name, &rel_group, &content_id).map_err(StepError::from_cache)? {
            if existing_db_pw.as_deref() != Some(saved_pw.as_str()) {
                if names_hidden {
                    rerun |= mask(archive::steps::LIST_STEP);
                }
                if probe_encrypted {
                    rerun |= mask(crate::probe::step::PROBE_STEP);
                }
            }
            writes.push(StepWrite::for_mutation(rel_id, StepMutation::SetNfoPassword { password: saved_pw }));
        }
        // an inline name stays as it is until the user opts in, as in the naming step. `applies` can't read this
        let r = ctx.release();
        if r.display_name.is_some()
            && r.name_source.is_none()
            && !ctx.data().rename_named(&r.group)?
        {
            return Ok(Outcome { writes, rerun, ..Outcome::default() });
        }
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let par2_names = ctx.data().par2_names(rel_id, &files)?;
        let check = ctx.data().local_password_check(rel_id)?;
        let mut pw_known = !writes.is_empty();
        // an NFO the probe or another step read is used as it was kept, not fetched again
        let mut kept = ctx.data().stored_nfo(rel_id)?;
        for file in archive::select::nfo_sources_with_par2(&files, &par2_names) {
            let par2_name = par2_names.get(&file.filename).map(String::as_str);
            let reused = match &kept {
                Some(k) if k.is_of(&file.filename, par2_name) => kept.take(),
                _ => None,
            };
            let (decoded, body) = match reused {
                Some(k) => (k.text, None),
                None => {
                    let Some(seg) = first_article(ctx, file)? else { continue };
                    let Some(body) = fetch_one(ctx, &seg, &file.filename)? else { continue };
                    let decoded = crate::probe::nfo_text(&body);
                    writes.push(crate::probe::nfo_store::keep(rel_id, &file.filename, &decoded));
                    (decoded, Some(body))
                }
            };
            if !pw_known
                && let Some(password) = release_info::find_verified_password([decoded.as_str()], &[], check.as_ref())
            {
                pw_known = true;
                crate::raw_store::put_nfo_password_checked(&rel_name, &rel_group, &content_id, &password).map_err(StepError::from_cache)?;
                if existing_db_pw.as_deref() != Some(password.as_str()) {
                    if names_hidden {
                        rerun |= mask(archive::steps::LIST_STEP);
                    }
                    if probe_encrypted {
                        rerun |= mask(crate::probe::step::PROBE_STEP);
                    }
                }
                writes.push(StepWrite::for_mutation(rel_id, StepMutation::SetNfoPassword { password }));
            }
            let bytes = body.as_deref().unwrap_or(decoded.as_bytes());
            let found = nfo::name_candidates(bytes).into_iter().find(|n| improves(ctx.release(), n, NameSource::Nfo));
            if let Some(name) = found {
                let change =
                    NameChange { display_name: Some(name), obfuscated: Some(false), source: NameSource::Nfo.value() };
                return Ok(Outcome { matched: true, name: Some(change), writes, rerun, ..Outcome::default() });
            }
        }
        Ok(Outcome { writes, rerun, ..Outcome::default() })
    }
}
