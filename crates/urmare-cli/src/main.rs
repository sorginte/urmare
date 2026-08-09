mod json;

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use thiserror::Error;
use urmare_core::{
    AnalysisError, DependencyPath, GitDiffAnalysis, GraphInspection, ImpactResult,
    ImportResolutionStatus, RepositoryAnalysis, display_repository_path,
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
    /// Repository root to analyze.
    #[arg(long, global = true, default_value = ".", value_name = "PATH")]
    root: PathBuf,

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
        after_help = "Examples:\n  urmare impact src/payments/service.py\n  urmare impact --git-diff main --json"
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
        after_help = "Examples:\n  urmare tests --affected src/payments/service.py\n  urmare tests --affected --git-diff main --json"
    )]
    Tests {
        /// Select affected tests, optionally for changed Python files.
        #[arg(long, value_name = "FILE", num_args = 0.., required = true)]
        affected: Option<Vec<PathBuf>>,
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
    /// Changed Python files, relative to the repository root; incompatible with --git-diff.
    #[arg(value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Graph {
            json,
            all,
            debug,
            focus,
        } => {
            let repository = RepositoryAnalysis::build(&cli.root)?;
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
            let ChangeSource { files, git_diff } = changes;
            let impact = match (files.is_empty(), git_diff) {
                (false, None) => RepositoryAnalysis::build(&cli.root)?.impact_many(&files)?,
                (true, Some(base)) => GitDiffAnalysis::build(&cli.root, &base)?.impact()?,
                (true, None) => return Err(AnalysisError::MissingChangedInput.into()),
                (false, Some(_)) => {
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
            git_diff,
            json,
        } => {
            let files = affected.unwrap_or_default();
            let impact = match (files.is_empty(), git_diff) {
                (false, None) => RepositoryAnalysis::build(&cli.root)?.impact_many(&files)?,
                (true, Some(base)) => GitDiffAnalysis::build(&cli.root, &base)?.impact()?,
                (true, None) => return Err(AnalysisError::MissingChangedInput.into()),
                (false, Some(_)) => {
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
            let repository = RepositoryAnalysis::build(&cli.root)?;
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
