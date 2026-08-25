mod json;

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use thiserror::Error;
use urmare_core::{
    AnalysisError, DependencyPath, GitDiffAnalysis, GraphInspection, ImpactResult,
    ImportResolutionStatus, RepositoryAnalysis, discover_git_repository_root,
    display_repository_path,
};

const LIST_LIMIT: usize = 25;

#[derive(Debug, Parser)]
#[command(
    name = "urmare",
    bin_name = "urmare",
    version,
    about = "Explain what follows from a Python code change",
    after_help = "Run 'urmare help <COMMAND>' for command-specific options."
)]
struct Cli {
    /// Repository root to analyze; Git-aware impact discovers it when omitted.
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build and summarize the local import graph.
    #[command(
        after_help = "Examples:\n  urmare graph\n  urmare graph --debug\n  urmare graph --json"
    )]
    Graph {
        /// Emit stable, schema-versioned JSON.
        #[arg(long)]
        json: bool,
        /// Show every human-readable unresolved import and debug result; incompatible with --json.
        #[arg(long, conflicts_with = "json")]
        all: bool,
        /// Show module mappings, resolved edges, and import-resolution traces.
        #[arg(long)]
        debug: bool,
        /// Restrict debug inspection to one indexed Python file; requires --debug.
        #[arg(long, value_name = "FILE", requires = "debug")]
        focus: Option<PathBuf>,
    },
    /// Calculate file-level blast radius from a path or Git changes.
    #[command(
        after_help = "Examples:\n  urmare impact src/payments/service.py\n  urmare impact --changed --json\n  urmare impact --git-diff main --json"
    )]
    Impact {
        #[command(flatten)]
        changes: ChangeSource,
        /// Emit stable, schema-versioned JSON.
        #[arg(long)]
        json: bool,
        /// Show every human-readable result; incompatible with --json.
        #[arg(long, conflicts_with = "json")]
        all: bool,
    },
    /// Select pytest-style files affected by a path or Git changes.
    #[command(
        after_help = "Examples:\n  urmare tests --affected src/payments/service.py\n  urmare tests --affected --changed\n  urmare tests --affected --git-diff main --json"
    )]
    Tests {
        /// Select affected tests, optionally for changed Python files.
        #[arg(long, value_name = "FILE", num_args = 0.., required = true)]
        affected: Option<Vec<PathBuf>>,
        /// Analyze staged, unstaged, and untracked Python changes against HEAD.
        #[arg(long)]
        changed: bool,
        /// Analyze Python changes since the merge base with this Git revision.
        #[arg(long, value_name = "BASE")]
        git_diff: Option<String>,
        /// Emit stable, schema-versioned JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain why an affected file depends on a changed file.
    #[command(
        after_help = "Examples:\n  urmare why src/payments/service.py tests/test_service.py\n  urmare why src/payments/service.py tests/test_service.py --json"
    )]
    Why {
        /// Changed dependency, relative to the repository root.
        #[arg(value_name = "CHANGED_FILE")]
        changed_file: PathBuf,
        /// Affected dependent, relative to the repository root.
        #[arg(value_name = "AFFECTED_FILE")]
        affected_file: PathBuf,
        /// Emit stable, schema-versioned JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
struct ChangeSource {
    /// Changed Python files, relative to the repository root; incompatible with --changed and --git-diff.
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,

    /// Analyze staged, unstaged, and untracked Python changes against HEAD.
    #[arg(long)]
    changed: bool,

    /// Analyze Python changes since the merge base with this Git revision instead of explicit files.
    #[arg(long, value_name = "BASE")]
    git_diff: Option<String>,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Analysis(#[from] AnalysisError),

    #[error("unable to serialize JSON output: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unable to write command output: {0}")]
    Output(#[from] io::Error),
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Json(_) | Self::Output(_) => 1,
            Self::Analysis(
                AnalysisError::InvalidGraph(_)
                | AnalysisError::MissingNodeMetadata(_)
                | AnalysisError::MissingModule(_)
                | AnalysisError::MissingEdgeProvenance { .. },
            ) => 1,
            Self::Analysis(
                AnalysisError::Config(_)
                | AnalysisError::MissingChangedInput
                | AnalysisError::ConflictingChangedInput,
            ) => 2,
            Self::Analysis(_) => 3,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    let Cli { root, command } = cli;
    match command {
        Command::Graph {
            json,
            all,
            debug,
            focus,
        } => {
            let root = selected_root(root.as_deref(), false)?;
            let repository = RepositoryAnalysis::build(&root)?;
            let summary = repository.summary();
            let inspection = debug
                .then(|| repository.graph_inspection(focus.as_deref()))
                .transpose()?;
            if json {
                write_json(&crate::json::graph(
                    &summary,
                    repository.unresolved_imports(),
                    inspection.as_ref(),
                )?)?;
            } else {
                print_graph(&summary, repository.unresolved_imports(), all);
                if let Some(inspection) = &inspection {
                    print_graph_inspection(inspection, all);
                }
            }
        }
        Command::Impact { changes, json, all } => {
            let ChangeSource {
                files,
                changed,
                git_diff,
            } = changes;
            let git_aware = changed || git_diff.is_some();
            let root = selected_root(root.as_deref(), git_aware)?;
            let impact = match (files.is_empty(), changed, git_diff) {
                (false, false, None) => RepositoryAnalysis::build(&root)?.impact_many(&files)?,
                (true, true, None) => GitDiffAnalysis::build(&root, "HEAD")?.impact()?,
                (true, false, Some(base)) => GitDiffAnalysis::build(&root, &base)?.impact()?,
                (true, false, None) => return Err(AnalysisError::MissingChangedInput.into()),
                _ => {
                    return Err(AnalysisError::ConflictingChangedInput.into());
                }
            };
            if json {
                write_json(&crate::json::impact(&impact)?)?;
            } else {
                print_impact(&impact, all);
            }
        }
        Command::Tests {
            affected,
            changed,
            git_diff,
            json,
        } => {
            let git_aware = changed || git_diff.is_some();
            let root = selected_root(root.as_deref(), git_aware)?;
            let files = affected.unwrap_or_default();
            let impact = match (files.is_empty(), changed, git_diff) {
                (false, false, None) => RepositoryAnalysis::build(&root)?.impact_many(&files)?,
                (true, true, None) => GitDiffAnalysis::build(&root, "HEAD")?.impact()?,
                (true, false, Some(base)) => GitDiffAnalysis::build(&root, &base)?.impact()?,
                (true, false, None) => return Err(AnalysisError::MissingChangedInput.into()),
                _ => {
                    return Err(AnalysisError::ConflictingChangedInput.into());
                }
            };
            if json {
                write_json(&crate::json::tests(&impact)?)?;
            } else {
                for path in impact.affected_tests {
                    println!("{}", display_repository_path(&path));
                }
            }
        }
        Command::Why {
            changed_file,
            affected_file,
            json,
        } => {
            let root = selected_root(root.as_deref(), false)?;
            let repository = RepositoryAnalysis::build(&root)?;
            let explanation = repository.why(&changed_file, &affected_file)?;
            if json {
                write_json(&crate::json::why(&explanation)?)?;
            } else {
                print_dependency_path(&explanation);
            }
        }
    }

    Ok(())
}

fn selected_root(root: Option<&Path>, discover_git: bool) -> Result<PathBuf, CliError> {
    let selected = root.unwrap_or_else(|| Path::new("."));
    if discover_git && root.is_none() {
        discover_git_repository_root(selected)
            .map_err(AnalysisError::from)
            .map_err(CliError::from)
    } else {
        Ok(selected.to_path_buf())
    }
}

fn write_json(output: &str) -> Result<(), io::Error> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(output.as_bytes())
}

fn print_graph(
    summary: &urmare_core::GraphSummary,
    unresolved_imports: &[urmare_core::UnresolvedImport],
    show_all: bool,
) {
    println!("Repository graph\n");
    println!("Python files        {:>6}", summary.python_files);
    println!("Modules             {:>6}", summary.modules);
    println!("Import edges        {:>6}", summary.import_edges);
    println!("Tests               {:>6}", summary.tests);
    println!("Unresolved imports  {:>6}", summary.unresolved_imports);

    if !unresolved_imports.is_empty() {
        println!("\nUnresolved import details ({})", unresolved_imports.len());
        println!("  No repository-local module matched; external packages are not resolved.");
        let shown = if show_all {
            unresolved_imports.len()
        } else {
            unresolved_imports.len().min(LIST_LIMIT)
        };
        for unresolved in unresolved_imports.iter().take(shown) {
            println!(
                "  {}:{}:{}  {}",
                display_repository_path(&unresolved.importer),
                unresolved.location.line,
                unresolved.location.column,
                unresolved.import,
            );
        }
        let omitted = unresolved_imports.len() - shown;
        if omitted > 0 {
            println!("  ... {omitted} more (use --all to show everything)");
        }
        println!("  Use --debug (optionally --focus <file>) to inspect resolution attempts.");
    }
}

fn print_graph_inspection(inspection: &GraphInspection, show_all: bool) {
    println!("\nGraph inspection");
    if let Some(focus) = &inspection.focus {
        println!("\nFocus\n  {}", display_repository_path(focus));
    }

    println!("\nSource roots ({})", inspection.source_roots.len());
    for root in &inspection.source_roots {
        if root.as_os_str().is_empty() {
            println!("  .");
        } else {
            println!("  {}", display_repository_path(root));
        }
    }

    println!("\nModule mappings ({})", inspection.modules.len());
    let shown_modules = shown_count(inspection.modules.len(), show_all);
    for module in inspection.modules.iter().take(shown_modules) {
        let kind = match (module.is_test, module.is_package) {
            (true, true) => "test package",
            (true, false) => "test",
            (false, true) => "package",
            (false, false) => "source",
        };
        println!(
            "  {} -> {} [{kind}; {} dependencies; {} dependents]",
            display_repository_path(&module.path),
            module.module,
            module.dependencies.len(),
            module.dependents.len(),
        );
    }
    print_omitted(inspection.modules.len(), shown_modules);

    println!("\nResolved import edges ({})", inspection.edges.len());
    let shown_edges = shown_count(inspection.edges.len(), show_all);
    for edge in inspection.edges.iter().take(shown_edges) {
        println!(
            "  {} -> {}",
            display_repository_path(&edge.dependent),
            display_repository_path(&edge.dependency),
        );
        for provenance in &edge.imports {
            println!(
                "    via {}:{}:{}  {}",
                display_repository_path(&edge.dependent),
                provenance.location.line,
                provenance.location.column,
                provenance.import,
            );
        }
    }
    print_omitted(inspection.edges.len(), shown_edges);

    println!(
        "\nImport resolution trace ({})",
        inspection.resolution_traces.len()
    );
    let shown_traces = shown_count(inspection.resolution_traces.len(), show_all);
    for trace in inspection.resolution_traces.iter().take(shown_traces) {
        let status = match trace.status {
            ImportResolutionStatus::Resolved => "resolved",
            ImportResolutionStatus::Unresolved => "unresolved",
            ImportResolutionStatus::InvalidRelativeImport => "invalid relative import",
        };
        println!(
            "  {}:{}:{}  {} [{status}]",
            display_repository_path(&trace.importer),
            trace.location.line,
            trace.location.column,
            trace.import,
        );
        if trace.candidate_modules.is_empty() {
            println!("    candidates: (none)");
        } else {
            println!("    candidates: {}", trace.candidate_modules.join(", "));
        }
        if trace.resolved_modules.is_empty() {
            println!("    matches: (none)");
        } else {
            for resolved in &trace.resolved_modules {
                println!(
                    "    matched {} -> {}",
                    resolved.module,
                    display_repository_path(&resolved.path),
                );
            }
        }
        if trace.status == ImportResolutionStatus::InvalidRelativeImport {
            println!("    reason: relative import ascends above the importer package");
        }
    }
    print_omitted(inspection.resolution_traces.len(), shown_traces);
}

fn shown_count(total: usize, show_all: bool) -> usize {
    if show_all {
        total
    } else {
        total.min(LIST_LIMIT)
    }
}

fn print_omitted(total: usize, shown: usize) {
    let omitted = total - shown;
    if omitted > 0 {
        println!("  ... {omitted} more (use --all to show everything)");
    }
}

fn print_impact(impact: &ImpactResult, show_all: bool) {
    let affected_tests: HashSet<&Path> =
        impact.affected_tests.iter().map(PathBuf::as_path).collect();
    let directly_affected_modules: Vec<_> = impact
        .directly_affected
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| !affected_tests.contains(path))
        .collect();
    let transitively_affected_modules: Vec<_> = impact
        .transitively_affected
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| !affected_tests.contains(path))
        .collect();
    let changed: Vec<_> = impact.changed.iter().map(PathBuf::as_path).collect();
    let tests: Vec<_> = impact.affected_tests.iter().map(PathBuf::as_path).collect();

    println!("Impact analysis");
    print_path_group("Changed", &changed, impact, false, show_all);

    println!("\nSummary");
    println!(
        "  Directly affected modules      {}",
        directly_affected_modules.len()
    );
    println!(
        "  Transitively affected modules  {}",
        transitively_affected_modules.len()
    );
    println!("  Affected tests                  {}", tests.len());

    print_path_group(
        "Directly affected modules",
        &directly_affected_modules,
        impact,
        true,
        show_all,
    );
    print_path_group(
        "Transitively affected modules",
        &transitively_affected_modules,
        impact,
        true,
        show_all,
    );
    print_path_group("Affected tests", &tests, impact, true, show_all);
}

fn print_path_group(
    label: &str,
    paths: &[&Path],
    impact: &ImpactResult,
    show_causes: bool,
    show_all: bool,
) {
    println!("\n{label} ({})", paths.len());
    if paths.is_empty() {
        println!("  (none)");
    } else {
        let shown = if show_all {
            paths.len()
        } else {
            paths.len().min(LIST_LIMIT)
        };
        for &path in paths.iter().take(shown) {
            println!("  {}", display_repository_path(path));
            if show_causes && impact.changed.len() > 1 {
                let causes = impact.causes_for(path);
                if !causes.is_empty() {
                    println!(
                        "    caused by {}",
                        causes
                            .iter()
                            .map(|cause| display_repository_path(cause))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        }
        let omitted = paths.len() - shown;
        if omitted > 0 {
            println!("  ... {omitted} more (use --all to show everything)");
        }
    }
}

fn print_dependency_path(explanation: &DependencyPath) {
    let mut paths = explanation.path.iter();
    if let Some(first) = paths.next() {
        println!("{}", display_repository_path(first));
    }
    for (path, step) in paths.zip(&explanation.steps) {
        println!("  -> {}", display_repository_path(path));
        for provenance in &step.imports {
            println!(
                "     via {}:{}:{}  {}",
                display_repository_path(&step.dependent),
                provenance.location.line,
                provenance.location.column,
                provenance.import,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use super::{AnalysisError, CliError};

    #[test]
    fn exit_codes_distinguish_internal_usage_and_analysis_failures() {
        let internal = CliError::Output(io::Error::other("output failed"));
        let usage = CliError::Analysis(AnalysisError::MissingChangedInput);
        let analysis =
            CliError::Analysis(AnalysisError::FileNotIndexed(PathBuf::from("missing.py")));

        assert_eq!(internal.exit_code(), 1);
        assert_eq!(usage.exit_code(), 2);
        assert_eq!(analysis.exit_code(), 3);
    }
}
