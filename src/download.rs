use std::time::Duration;

use anyhow::Result;

use crate::nzb::{generate_nzb, nzb_filename};
use crate::sab;
use crate::search::{get_articles, get_release};
use crate::ui;

/// Hand a release to SABnzbd via its watched folder. true when it got queued.
pub fn download_release(release_id: i64) -> Result<bool> {
    let storage=crate::config::checked_storage_config().map_err(anyhow::Error::msg)?;
    if storage.is_postgres() {
        return after_native_preflight(||crate::pg::fresh_nzb_tools::download_metadata(&storage,release_id),|(name,first,download)|{
            anyhow::ensure!(crate::config::checked_storage_config().map_err(anyhow::Error::msg)?==storage,"storage destination changed during download preparation");
            send_to_sab(release_id,&storage,name,first,Some(download))
        });
    }
    let Some(release)=get_release(release_id)? else{ui::error("Release not found");return Ok(false)};
    let articles=get_articles(release_id)?;
    if articles.is_empty(){ui::error("No articles found");return Ok(false);}
    let first=articles.iter().find_map(|article|article.filename.as_deref().filter(|name|!name.is_empty())).map(str::to_owned);
    send_to_sab(release_id,&storage,release.name,first,None)
}

fn after_native_preflight(
    prepare:impl FnOnce()->Result<Option<crate::pg::fresh_nzb_tools::PreparedDownload>>,
    handoff:impl FnOnce(crate::pg::fresh_nzb_tools::PreparedDownload)->Result<bool>,
)->Result<bool> {
    let Some(download)=prepare()? else{ui::error("Release or articles not found");return Ok(false)};
    handoff(download)
}

fn send_to_sab(release_id:i64,storage:&crate::pg::StorageConfig,release_name:String,first_file:Option<String>,prepared:Option<crate::pg::fresh_nzb::Download>)->Result<bool> {
    anyhow::ensure!(storage.is_postgres()==prepared.is_some(),"download source does not match the selected backend");
    let complete_dir=sab::get_complete_dir();
    if first_file.as_deref().is_some_and(|name|complete_dir.join(name).exists()) {
        ui::warn("already downloaded");return Ok(false);
    }

    sab::configure_servers();

    if !sab::is_running() {
        ui::dim("starting sabnzbd...");
        if !sab::start() {
            ui::error("couldnt start sabnzbd");
            return Ok(false);
        }
    }

    if !sab::wait_ready(Duration::from_secs(90)) {
        ui::warn("sab isnt ready");
        return Ok(false);
    }

    let watched_dir = sab::configure_watched_dir();
    let release_name=if release_name.is_empty(){format!("release_{release_id}")}else{release_name};
    let filename=nzb_filename(&release_name,Some(release_id));

    if watched_dir.join(&filename).exists() {
        ui::warn("already queued");
        return Ok(false);
    }

    anyhow::ensure!(&crate::config::checked_storage_config().map_err(anyhow::Error::msg)?==storage,"storage destination changed during download preparation");
    if let Some(download)=prepared {crate::pg::fresh_nzb_tools::publish_download(&release_name,release_id,Some(&watched_dir),download)?;}
    else{generate_nzb(release_id,Some(&watched_dir))?;}

    match sab::job_in_sab(filename.strip_suffix(".nzb").unwrap_or(&filename), Duration::from_secs(10)).as_deref() {
        Some("queued") => {
            ui::success("download queued");
            let _ = webbrowser::open(&sab::get_url());
            Ok(true)
        }
        Some(status) => {
            ui::warn(&format!("already did this one bruh: {status}"));
            Ok(false)
        }
        None => {
            ui::error("couldnt confirm it");
            ui::error("download failed");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod native_preflight_tests {
    use super::*;
    use crate::{parser::{Article,Release},pg::{self,fresh_reader::{ReadView,Budget}}};
    use std::{cell::Cell,time::Instant};
    #[test]
    #[ignore="requires explicit isolated fresh PostgreSQL schema and owned bounded NZB spool"]
    fn native_download_refuses_invalid_inputs_before_handoff_and_publishes_the_preflight_snapshot() {
        let schema=pg::testing::Schema::new().expect("explicit isolated fresh schema");let mut client=schema.pool.get().unwrap();
        for (n,(name,file,total)) in [("Complete.字幕.Release",Some("video.字幕.\"&.mkv"),1),("Incomplete.Preflight.Release",Some("video.mkv"),3),
            ("Uncaptured.Empty.Preflight",Some(""),1),("No.Articles.Preflight",Some("video.mkv"),1)].into_iter().enumerate() {
            let saved=pg::release_writer::save(&mut client,&[Release {name:name.into(),group:"alt.binaries.test".into(),poster:"original & poster".into(),
                date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:file.map(str::to_owned),subject:if n==0 {"original video.字幕.\"&.mkv yEnc (1/1)"}else{"original subject"}.into(),
                    message_id:format!("<preflight-{n}@example.invalid>"),part:Some(1),total_parts:Some(total),bytes:17,..Default::default()}],..Default::default()}],1).unwrap();
            assert_eq!((saved.releases,saved.files,saved.parts),(1,1,1));
        }
        let ids=client.query("select name,id from releases",&[]).unwrap().into_iter().map(|row|(row.get::<_,String>(0),row.get::<_,i64>(1))).collect::<std::collections::HashMap<_,_>>();
        client.execute("delete from segments where release_id=$1",&[&ids["No.Articles.Preflight"]]).unwrap();
        let root=tempfile::tempdir().unwrap();
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(root.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
        let pool=crate::nzb_spool::Pool::new(root.path().into(),crate::nzb_spool::Limits {requests:5,per_request_bytes:1<<20,total_bytes:5<<20,free_bytes:0}).unwrap();
        let effects=Cell::new(0usize);
        for (id,invalid) in [(ids["Incomplete.Preflight.Release"],true),(ids["Uncaptured.Empty.Preflight"],true),(ids["No.Articles.Preflight"],false),(i64::MAX,false)] {
            let result=after_native_preflight(|| {
                let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10))?;
                view.prepare_download(id,&pool)
            },|_|{effects.set(effects.get()+1);Ok(true)});
            if invalid {assert!(result.is_err(),"invalid native NZB must fail before a SAB handoff");}
            else {assert!(!result.unwrap(),"missing release/articles must not enter a SAB handoff");}
            assert_eq!(effects.get(),0,"SAB configuration/start/watched-folder work belongs after this boundary");
        }
        let output=tempfile::tempdir().unwrap();let good=ids["Complete.字幕.Release"];
        let prepared={
            let view=ReadView::open(&mut client,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            view.prepare_download(good,&pool)
        };
        let result=after_native_preflight(||prepared,|(name,first,download)| {
            effects.set(effects.get()+1);assert_eq!(name,"Complete.字幕.Release");assert_eq!(first.as_deref(),Some("video.字幕.\"&.mkv"));
            assert!(download.body.is_complete());
            // This mutation occurs after the readonly preflight transaction has
            // ended. Publishing must use that completed body, not query again.
            client.execute("delete from segments where release_id=$1",&[&good])?;
            let target=pg::fresh_nzb_tools::publish_download(&name,good,Some(output.path()),download)?;
            let xml=std::fs::read_to_string(&target)?;
            assert_eq!(xml.matches("<file ").count(),1);assert_eq!(xml.matches("<segment bytes=").count(),1);
            assert!(xml.contains("preflight-0@example.invalid")&&xml.contains("original &amp; poster"),"{xml}");
            assert!(xml.contains("subject=\"original video.字幕.&quot;&amp;.mkv yEnc (1/1)\""),"the accepted original subject must survive unchanged: {xml}");
            assert_eq!(target.parent(),Some(output.path()));Ok(true)
        }).unwrap();
        assert!(result);assert_eq!(effects.get(),1);
    }
}
