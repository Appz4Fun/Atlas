//! Explicit captured-source conversion CLI; never a runtime activation path.
use super::history_import::{ExpectedDestination, ImportBudget, ImportOwner};
use crate::history_identity::{ErrorCode, HistoryError};
use crate::history_source::{Manifest, Source, VerificationAdmission};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn document<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, HistoryError::new(ErrorCode::InsufficientBudget, "CLI document admission"));
    Ok(serde_json::from_slice(&bytes)?)
}
fn run(args: &[String]) -> Result<()> {
    let mut options = BTreeMap::new();
    let mut apply = false;
    let mut plaintext = false;
    let mut i = 0;
    const VALUES: &[&str] = &[
        "--manifest",
        "--verify-bytes",
        "--verify-rate",
        "--source-open-files",
        "--source-cache-bytes",
        "--source-schema-bytes",
        "--destination",
        "--url-env",
        "--spool",
        "--spool-quota",
        "--source-rows",
        "--source-bytes",
        "--copy-bytes",
        "--mutations",
        "--chunk-bytes",
        "--decoder-window-log",
        "--decoder-window-bytes",
        "--max-pages",
    ];
    while i < args.len() {
        match args[i].as_str() {
            "--import-history" => {}
            "--apply" => apply = true,
            "--allow-plaintext" => plaintext = true,
            flag if VALUES.contains(&flag) => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.starts_with("--"))
                    .context(HistoryError::new(ErrorCode::UnsupportedSchema, "missing CLI value"))?;
                ensure!(
                    options.insert(flag, value.as_str()).is_none(),
                    HistoryError::new(ErrorCode::UnsupportedSchema, "duplicate CLI option")
                );
                i += 1;
            }
            _ => bail!(HistoryError::new(ErrorCode::UnsupportedSchema, "unknown historical CLI option")),
        }
        i += 1;
    }
    let required = |flag| {
        options
            .get(flag)
            .copied()
            .with_context(|| HistoryError::new(ErrorCode::UnsupportedSchema, format!("{flag} is required")))
    };
    let number = |flag, default: u64| -> Result<u64> {
        Ok(options.get(flag).map(|v| v.parse()).transpose()?.unwrap_or(default))
    };
    let manifest: Manifest = document(Path::new(required("--manifest")?), 8 * 1024 * 1024)?;
    let mut verification = VerificationAdmission::new(required("--verify-bytes")?.parse()?);
    verification.bytes_per_second = number("--verify-rate", 32 * 1024 * 1024)?;
    verification.max_open_files = u32::try_from(number("--source-open-files", 33)?)?;
    verification.sqlite_cache_bytes = number("--source-cache-bytes", 16 * 1024 * 1024)?;
    verification.max_schema_bytes = number("--source-schema-bytes", 16 * 1024 * 1024)?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let source = Source::open_with_admission(manifest, verification, &cancel)?;
    if !apply {
        println!(
            "{}",
            serde_json::json!({"state":"verified_source","verified_bytes":source.verified_bytes,"activation":false})
        );
        return Ok(());
    }
    let expected: ExpectedDestination = document(Path::new(required("--destination")?), 16384)?;
    ensure!(
        expected.staging_prefix == "atlas_stage_",
        HistoryError::new(ErrorCode::DestinationMismatch, "CLI requires atlas_stage_ destination")
    );
    let name = required("--url-env")?;
    ensure!(
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
        HistoryError::new(ErrorCode::DestinationMismatch, "explicit URL environment name")
    );
    let url = std::env::var(name)
        .context(HistoryError::new(ErrorCode::DestinationMismatch, "destination URL environment is missing"))?;
    let storage = super::StorageConfig {
        backend: super::Backend::Postgres,
        url,
        schema: expected.schema.clone(),
        allow_plaintext: plaintext,
        max_connections: 1,
    };
    let spool = PathBuf::from(required("--spool")?);
    let quota = required("--spool-quota")?.parse()?;
    let mut owner = ImportOwner::connect(storage, expected, spool, quota)?;
    let decoder = crate::history_stream::Config {
        window_log_max: u32::try_from(number("--decoder-window-log", 27)?)?,
        ..Default::default()
    };
    owner.configure_decoder(decoder, number("--decoder-window-bytes", 128 * 1024 * 1024)?)?;
    let mut budget = ImportBudget::default();
    budget.source_rows = u32::try_from(number("--source-rows", u64::from(budget.source_rows))?)?;
    budget.source_bytes = number("--source-bytes", budget.source_bytes)?;
    budget.copy_bytes = number("--copy-bytes", budget.copy_bytes)?;
    budget.mutations = u32::try_from(number("--mutations", u64::from(budget.mutations))?)?;
    budget.chunk_bytes = u32::try_from(number("--chunk-bytes", u64::from(budget.chunk_bytes))?)?;
    let max_pages = number("--max-pages", u64::MAX)?;
    ensure!(max_pages > 0, HistoryError::new(ErrorCode::InsufficientBudget, "max-pages must be positive"));
    budget.check()?;
    let id = owner.begin(&source)?;
    let (state, mut sequence) = owner.status(id)?;
    ensure!(
        state != "failed",
        HistoryError::new(ErrorCode::IncompleteImport, "failed import requires owner investigation")
    );
    if state == "identity_ready" {
        println!("{}", serde_json::json!({"state":state,"sequence":sequence,"activation":false}));
        return Ok(());
    }
    for _ in 0..max_pages {
        let receipt = owner.step(&source, id, sequence, budget)?;
        sequence = receipt.sequence;
        if receipt.complete {
            owner.finish_identity(&source, id)?;
            println!(
                "{}",
                serde_json::json!({"state":"identity_ready","sequence":sequence,"activation":false,"last_step":receipt})
            );
            return Ok(());
        }
    }
    println!("{}", serde_json::json!({"state":"loading","sequence":sequence,"activation":false}));
    Ok(())
}
pub fn run_cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("history import: {error:#}");
            1
        }
    }
}
