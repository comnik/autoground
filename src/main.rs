use std::path::{Path, PathBuf};
use std::process::ExitCode;

use autoground::edb::Store;
use autoground::eval::run;
use autoground::navigator::judge::epoch_ms;
use autoground::navigator::{FixtureFile, FixtureJudge, JevJudge, RecordingJudge, TreeNavigator};
use autoground::report::{
    allocate_record_path, format_require_traces, format_run_body, format_run_header,
    jev_request_count, require_traces, run_exit_code, RunTiming,
};
use autoground::{compile, parse, verbalize_source};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "autoground",
    about = "Self-grounding conceptual models of a codebase"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum JudgeKind {
    /// TypeSafe Jev via TYPESAFE_API_KEY (default).
    Jev,
    /// Local stand-in. No API key.
    Heuristic,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse a model and evaluate its requires against EDB, grounding from a
    /// repository via the navigator (default: the current directory).
    ///
    /// Prints the evaluation result and grounding. On a TTY, left/right opens
    /// the step-by-step debug view. A judge recording is always written under
    /// `/tmp/autoground/`.
    ///
    /// Examples:
    ///   autoground run examples/layered.ag --repo examples/toy-layered
    ///   autoground run --repo examples/toy-split '(require (component "Frontend"))'
    ///   autoground run --inspect /tmp/autoground/trace-….json
    Run {
        /// Path to a `.ag` model, or a model source string.
        #[arg(required_unless_present = "inspect")]
        model: Option<String>,
        /// Repository to ground against.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        #[arg(long)]
        fixtures: Option<PathBuf>,
        /// Reopen a recorded trace and render it as if the run had just completed.
        #[arg(long, value_name = "TRACE")]
        inspect: Option<PathBuf>,
    },
    /// Verbalize an atom or a model using `declare` templates.
    ///
    /// Examples:
    ///   autoground verbalize '(Component "Frontend")'
    ///   autoground verbalize examples/layered.ag
    Verbalize {
        /// Atom, model source, or path to a `.ag` file.
        input: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run {
            model,
            repo,
            judge,
            fixtures,
            inspect,
        } => {
            let result = if let Some(path) = inspect {
                inspect_trace(&path)
            } else {
                run_model(
                    model.as_deref().expect("model required"),
                    &repo,
                    judge,
                    fixtures.as_ref(),
                )
            };
            match result {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(3)
                }
            }
        }
        Commands::Verbalize { input } => match verbalize_cmd(&input) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
    }
}

fn verbalize_cmd(input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let src = model_source(input)?;
    for line in verbalize_source(&src)? {
        println!("{line}");
    }
    Ok(())
}

fn run_model(
    model: &str,
    repo: &Path,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let started_at = std::time::SystemTime::now();
    let src = model_source(model)?;
    let program = parse(&src)?;
    let compiled = compile(&program)?;
    let mut store = Store::new();
    let repo_name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let record_path = allocate_record_path()?;

    let mut nav = make_nav(repo, judge, fixtures, &record_path, model)?;
    let result = run(&compiled, &mut store, Some(&mut nav));
    let ended_at = std::time::SystemTime::now();
    let judge_failed = !nav.errors.is_empty();
    if judge_failed {
        for e in &nav.errors {
            eprintln!("judge error: {e}");
        }
    }
    let steps = std::mem::take(&mut nav.trace);
    drop(nav);

    write_run_trace(
        &record_path,
        &result.outcome,
        repo_name,
        require_traces(&compiled, &result),
        &steps,
        started_at,
        ended_at,
    )?;

    let header = format_run_header(&result.outcome, &record_path, jev_request_count(&steps));
    let body = format_run_body(&compiled, &result);
    autoground::report::tui::present_run(
        &header,
        &body,
        &steps,
        repo_name,
        RunTiming {
            started_at,
            ended_at,
        },
    )?;
    if judge_failed {
        Ok(3)
    } else {
        Ok(run_exit_code(&result.outcome))
    }
}

fn inspect_trace(path: &Path) -> Result<i32, Box<dyn std::error::Error>> {
    let file = FixtureFile::load(path)
        .map_err(|e| format!("could not read trace {}: {e}", path.display()))?;
    let outcome = file.outcome.clone().ok_or_else(|| {
        format!(
            "{} is a judge fixture, not a full run trace; re-run to record one",
            path.display()
        )
    })?;
    let timing = match (file.started_at_ms, file.ended_at_ms) {
        (Some(s), Some(e)) => RunTiming::from_millis(s, e),
        _ => {
            let now = std::time::SystemTime::now();
            RunTiming {
                started_at: now,
                ended_at: now,
            }
        }
    };
    let header = format_run_header(&outcome, path, jev_request_count(&file.steps));
    let body = format_require_traces(&file.requires);
    autoground::report::tui::present_run(&header, &body, &file.steps, &file.repo_name, timing)?;
    Ok(run_exit_code(&outcome))
}

fn write_run_trace(
    path: &Path,
    outcome: &autoground::Outcome,
    repo_name: &str,
    requires: Vec<autoground::RequireTrace>,
    steps: &[autoground::GroundStep],
    started_at: std::time::SystemTime,
    ended_at: std::time::SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = FixtureFile::load(path).unwrap_or_default();
    file.outcome = Some(outcome.clone());
    file.repo_name = repo_name.to_string();
    file.requires = requires;
    file.steps = steps.to_vec();
    file.started_at_ms = Some(epoch_ms(started_at));
    file.ended_at_ms = Some(epoch_ms(ended_at));
    file.save(path)?;
    Ok(())
}

/// Read `input` as a `.ag` file when it names one, otherwise treat it as source.
fn model_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if path.is_file() {
        return Ok(std::fs::read_to_string(path)?);
    }
    let trimmed = input.trim_start();
    if trimmed.starts_with('(') {
        return Ok(input.to_string());
    }
    if input.ends_with(".ag") || path.exists() {
        return Err(format!("model file not found: {input}").into());
    }
    Ok(input.to_string())
}

fn make_nav(
    repo: &Path,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
    record: &Path,
    model: &str,
) -> Result<TreeNavigator, Box<dyn std::error::Error>> {
    let inner: Box<dyn autoground::navigator::Judge> = if let Some(path) = fixtures {
        Box::new(FixtureJudge::load(path)?)
    } else {
        match judge {
            JudgeKind::Heuristic => Box::new(autoground::HeuristicJudge),
            JudgeKind::Jev => Box::new(JevJudge::from_env()?),
        }
    };
    let mut nav = TreeNavigator::new(repo, Box::new(RecordingJudge::new(inner, record)));
    let model_path = Path::new(model);
    if model_path.is_file() {
        nav.hide_model(model_path);
    }
    Ok(nav)
}
