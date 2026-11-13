//! The actual Step/provider settings captured by one native worker attempt.
use anyhow::{Result,ensure};
use crate::config::Config;

pub(crate) struct ConfigPin {
    assets:Option<crate::config::NativeAssetsConfig>,spots:crate::config::SpotsConfig,
    postprocess:crate::config::PostprocessConfig,archive:crate::config::ArchiveConfig,
    naming:crate::config::NamingConfig,predb:crate::config::PredbConfig,
    category:crate::config::CategoryConfig,media:crate::config::MediaConfig,
    servers:Vec<crate::config::UsenetServer>,plan:Vec<(usize,usize)>,unsaved:usize,
}
impl ConfigPin {
    pub(crate) fn capture(config:&Config,servers:&[crate::config::UsenetServer],plan:&[(usize,usize)])->Self {
        Self {assets:config.native_assets.clone(),spots:config.spots.clone(),postprocess:config.postprocess.clone(),archive:config.archive.clone(),naming:config.naming.clone(),
            predb:config.predb.clone(),category:config.category.clone(),media:config.media.clone(),
            servers:servers.to_vec(),plan:plan.to_vec(),unsaved:config.max_unsaved_headers()}
    }
    pub(crate) fn check(&self,config:&Config,servers:&[crate::config::UsenetServer],plan:&[(usize,usize)])->Result<()> {
        ensure!(self.assets==config.native_assets && self.spots==config.spots && self.postprocess==config.postprocess && self.archive==config.archive && self.naming==config.naming
            && self.predb==config.predb && self.category==config.category && self.media==config.media
            && self.servers==servers && self.plan==plan && self.unsaved==config.max_unsaved_headers(),
            "native worker configuration changed; restart with the intended worker settings");Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_reload_pins_inferred_lane_and_platform_refuses_instead_of_skipping() {
        let config=Config::default();let pin=ConfigPin::capture(&config,&[],&[]);let mut changed=config.clone();
        changed.inferred_parts=!changed.inferred_parts;
        assert!(pin.check(&changed,&[],&[]).unwrap_err().to_string().contains("restart"));
        assert!(super::super::workers::check_inferred_platform(false).is_ok());
        #[cfg(unix)] assert!(super::super::workers::check_inferred_platform(true).is_ok());
        #[cfg(not(unix))] assert!(super::super::workers::check_inferred_platform(true).unwrap_err().to_string().contains("Unix"));
    }

    #[test]
    fn native_reload_pins_both_spotnet_lanes_before_legacy_reconfiguration() {
        let config=Config::default();let pin=ConfigPin::capture(&config,&[],&[]);
        let edits:[fn(&mut Config);5]=[
            |c|c.spots.enabled=!c.spots.enabled,|c|c.spots.comments=!c.spots.comments,
            |c|c.spots.concurrency+=1,|c|c.spots.comments_per_pass+=1,
            |c|c.spots.blacklist_url="https://private.invalid/changed-policy".into(),
        ];
        for edit in edits {
            let mut changed=config.clone();edit(&mut changed);let error=pin.check(&changed,&[],&[]).unwrap_err();
            assert!(error.to_string().contains("restart"));assert!(!error.to_string().contains("private.invalid"));
        }
        assert!(pin.check(&config,&[],&[]).is_ok());
    }

    #[test]
    fn native_reload_refuses_changed_captured_step_and_provider_settings_without_values() {
        let config=Config::default();let pin=ConfigPin::capture(&config,&[],&[]);
        let edits:[fn(&mut Config);6]=[
            |c|c.postprocess.body_fetches_per_minute+=1,|c|c.archive.probe_files+=1,
            |c|c.naming.nfo_step=!c.naming.nfo_step,|c|c.predb.poll_minutes+=1,
            |c|c.category.foreign=!c.category.foreign,|c|c.media.opensubtitles.api_key="private-test-key".into(),
        ];
        for edit in edits {
            let mut changed=config.clone();edit(&mut changed);let error=pin.check(&changed,&[],&[]).unwrap_err();
            assert!(error.to_string().contains("restart"));assert!(!error.to_string().contains("private-test-key"));
            assert_eq!(crate::pg::outage::classify(&error),crate::pg::outage::Failure::Fatal);
        }
        assert!(pin.check(&config,&[],&[]).is_ok());
    }
    #[test]
    fn native_reload_allows_unrelated_reader_group_and_retired_freeze_settings() {
        let config=Config::default();let pin=ConfigPin::capture(&config,&[],&[]);let mut changed=config.clone();
        changed.api_port=Some(12345);changed.api_host="127.0.0.1".into();changed.groups.push("example.group".into());
        changed.index_mode="live".into();changed.freeze.enabled=!changed.freeze.enabled;
        assert!(pin.check(&changed,&[],&[]).is_ok());
        // The pin performs no PG operation and cannot rewrite an independently
        // typed outage into a configuration failure.
        let unavailable=anyhow::Error::new(crate::pg::outage::SessionUnavailable);
        assert_eq!(crate::pg::outage::classify(&unavailable),crate::pg::outage::Failure::Unavailable);
    }
    #[test]
    fn native_reload_refuses_stale_captured_pool_without_logging_account_values() {
        let config=Config::default();let pin=ConfigPin::capture(&config,&[],&[]);
        let server=crate::config::UsenetServer::new("example.test","test-user","private-test-password",119);
        let error=pin.check(&config,&[server],&[(0,1)]).unwrap_err();
        assert!(!error.to_string().contains("test-user"));assert!(!error.to_string().contains("private-test-password"));
        assert!(pin.check(&config,&[],&[(0,1)]).is_err());
        let mut changed=config.clone();changed.max_unsaved_headers=Some(1);
        assert!(pin.check(&changed,&[],&[]).is_err());
    }
    #[test]
    fn native_reload_pins_asset_selection_without_exposing_paths() {
        let mut config=Config::default();
        config.native_assets=Some(crate::config::NativeAssetsConfig {root:"/retained-assets".into(),manifest:"/evidence/assets.jsonl".into(),whole_sha256:"01".repeat(32)});
        let pin=ConfigPin::capture(&config,&[],&[]);
        assert!(pin.check(&config,&[],&[]).is_ok());
        let mut changed=config.clone();changed.native_assets.as_mut().unwrap().manifest="/other-manifest".into();
        let error=pin.check(&changed,&[],&[]).unwrap_err();
        assert!(!error.to_string().contains("retained-assets"));assert!(!error.to_string().contains("other-manifest"));
        changed.native_assets=None;assert!(pin.check(&changed,&[],&[]).is_err());
    }

}
