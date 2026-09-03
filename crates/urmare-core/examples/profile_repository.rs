//! Release-mode profiling helper for the controlled real-project benchmark.
//!
//! This deliberately lives outside the public `urmare` CLI contract. The
//! benchmark runner measures that CLI independently and invokes this helper on
//! a separate cache to capture internal phase timings and work counters.

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use urmare_core::{
    AnalysisTimings, FullValidationReason, ImpactResult, IndexBuildKind, IndexFallbackReason,
    IndexWorkStats, QueryProfile, RepositoryAnalysis, display_repository_path,
};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse(env::args_os().skip(1))?;
    let (repository, analysis) = if arguments.uncached {
        RepositoryAnalysis::build_uncached_profiled(&arguments.root)?
    } else {
        let Some(cache) = arguments.cache.as_deref() else {
            return Err("--cache is required unless --uncached is used".into());
        };
        RepositoryAnalysis::build_profiled_with_cache_directory(&arguments.root, cache)?
    };
    let summary = repository.summary();
    let (impact, query) = repository.impact_profiled(&arguments.changed)?;

    let output = json!({
        "schema_version": 1,
        "urmare_version": env!("CARGO_PKG_VERSION"),
        "build": {
            "git_commit": env!("URMARE_BUILD_GIT_COMMIT"),
            "rustc_version": env!("URMARE_BUILD_RUSTC_VERSION"),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "repository": {
            "python_files": summary.python_files,
            "modules": summary.modules,
            "import_edges": summary.import_edges,
            "tests": summary.tests,
            "unresolved_imports": summary.unresolved_imports,
        },
        "internal_timings_ns": analysis_timings(&analysis),
        "internal_work": index_work(&analysis.index_work),
        "query_profile": query_profile(&query),
        "result": impact_value(&impact),
    });
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

#[derive(Debug)]
struct Arguments {
    root: PathBuf,
    cache: Option<PathBuf>,
    changed: PathBuf,
    uncached: bool,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut root = None;
        let mut cache = None;
        let mut changed = None;
        let mut uncached = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--root") => root = Some(next_path(&mut arguments, "--root")?),
                Some("--cache") => cache = Some(next_path(&mut arguments, "--cache")?),
                Some("--changed") => changed = Some(next_path(&mut arguments, "--changed")?),
                Some("--uncached") => uncached = true,
                Some(other) => return Err(format!("unknown argument `{other}`")),
                None => return Err("arguments must be valid Unicode".to_owned()),
            }
        }
        if uncached && cache.is_some() {
            return Err("--cache and --uncached are mutually exclusive".to_owned());
        }
        if !uncached && cache.is_none() {
            return Err("--cache is required unless --uncached is used".to_owned());
        }
        Ok(Self {
            root: root.ok_or_else(|| "--root is required".to_owned())?,
            cache,
            changed: changed.ok_or_else(|| "--changed is required".to_owned())?,
            uncached,
        })
    }
}

fn next_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{option} requires a path"))
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap_or(u64::MAX)
}

fn analysis_timings(timings: &AnalysisTimings) -> Value {
    json!({
        "index_load": nanos(timings.index_timings.load),
        "git_delta_detection": nanos(timings.index_timings.delta_detection),
        "update": nanos(timings.index_timings.update),
        "persistence": nanos(timings.index_timings.persistence),
        "index_total": nanos(timings.total()),
    })
}

fn query_profile(profile: &QueryProfile) -> Value {
    json!({
        "index_open": nanos(profile.index_load),
        "impact_query": nanos(profile.query),
        "recovery_fallback": nanos(profile.fallback_rebuild),
        "query_total": nanos(profile.total()),
        "records_read": profile.index_records_read,
    })
}

fn index_work(work: &IndexWorkStats) -> Value {
    json!({
        "directories_inspected": work.directories_inspected,
        "inventory_entries_inspected": work.inventory_entries_inspected,
        "files_statted": work.files_statted,
        "files_read": work.files_read,
        "files_hashed": work.files_hashed,
        "files_parsed": work.files_parsed,
        "modules_added": work.modules_added,
        "modules_removed": work.modules_removed,
        "modules_remapped": work.modules_remapped,
        "modules_reused": work.modules_reused,
        "importers_reresolved": work.importers_reresolved,
        "records_added": work.records_added,
        "records_removed": work.records_removed,
        "forward_edges_added": work.forward_edges_added,
        "forward_edges_removed": work.forward_edges_removed,
        "reverse_edges_added": work.reverse_edges_added,
        "reverse_edges_removed": work.reverse_edges_removed,
        "index_records_read": work.index_records_read,
        "index_records_written": work.index_records_written,
        "bytes_written": work.bytes_written,
        "build_kind": build_kind(work.build_kind),
        "fallback_reason": work.fallback_reason.map(fallback_reason),
    })
}

fn build_kind(kind: IndexBuildKind) -> &'static str {
    match kind {
        IndexBuildKind::Full => "full",
        IndexBuildKind::Incremental => "incremental",
        IndexBuildKind::Reused => "reused",
    }
}

fn fallback_reason(reason: IndexFallbackReason) -> &'static str {
    match reason {
        IndexFallbackReason::CacheDisabled => "cache_disabled",
        IndexFallbackReason::MissingIndex => "missing_index",
        IndexFallbackReason::IncompatibleIndex => "incompatible_index",
        IndexFallbackReason::ConfigurationChanged => "configuration_changed",
        IndexFallbackReason::SourceRootRemapped => "source_root_remapped",
        IndexFallbackReason::NonGitRepository => "non_git_repository",
        IndexFallbackReason::GitStateUnavailable => "git_state_unavailable",
        IndexFallbackReason::IndexLocked => "index_locked",
        IndexFallbackReason::IndexCorrupt => "index_corrupt",
        IndexFallbackReason::StorageFailure => "storage_failure",
    }
}

fn impact_value(impact: &ImpactResult) -> Value {
    let full_validation = impact.full_validation.as_ref().map(|validation| {
        json!({
            "required": true,
            "reason": match validation.reason {
                FullValidationReason::ConfigurationChanged => "configuration_changed",
            },
            "configuration_paths": paths(&validation.configuration_paths),
        })
    });
    json!({
        "schema_version": 1,
        "changed": paths(&impact.changed),
        "directly_affected": paths(&impact.directly_affected),
        "transitively_affected": paths(&impact.transitively_affected),
        "affected_tests": paths(&impact.affected_tests),
        "full_validation": full_validation,
        "attributions": impact.attributions.iter().map(|attribution| json!({
            "affected": display_repository_path(&attribution.affected),
            "caused_by": paths(&attribution.caused_by),
        })).collect::<Vec<_>>(),
    })
}

fn paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| display_repository_path(Path::new(path)))
        .collect()
}
