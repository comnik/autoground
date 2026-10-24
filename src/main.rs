use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use autoground::align::{lower_mermaid, mismatch, model_of};
use autoground::check::{self, write_report, DEFAULT_BODY_BYTES};
use autoground::compiler::{link_parts, Vocabulary};
use autoground::log::{Log, DEFAULT_TRACE_DIR};
use autoground::oracle::jev::DEFAULT_RUN_COST_CENTS;
use autoground::oracle::{JevOracle, Oracle, UsageRecord};
use autoground::report::context::write_context;
use autoground::report::summary::{print_run_summary, print_sweep_summary};
use autoground::sweep;
use autoground::{brief_source, compile, display_path, parse, scan_namespaces, Proposition};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "autoground", about = "Check a brief, or sweep for matches")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Re-check a brief against the files its claims cite.
    ///
    /// A witness path is relative to the brief file. A file longer than the
    /// body budget is sent in parts, and the sentence is discharged when one
    /// part entails it. A claim is discharged when its support holds and,
    /// where that support is a sentence, those sentences entail the claim. An
    /// assumption is taken as true. A detail is discharged when its body is.
    /// `--budget` caps this run in cents and defaults to 1. On a terminal the
    /// spinner names the sentence in flight, and the finished view is the
    /// brief: each judged clause carries its own mark, and an undischarged
    /// clause carries its failure comment. The trace and the context are
    /// written in a new directory under `/tmp/autoground/`, or under `--out`.
    /// Exit status is 1 when a top-level claim is not discharged.
    ///
    /// Examples:
    ///   autoground check examples/search-calls-oracle.brief
    ///   autoground check examples/sqrt2.brief
    Check {
        /// Brief source, a `.brief` file, or a Markdown file of `brief` blocks.
        input: String,
        /// File every `(witness $)` reads. Other witness paths stay relative to the brief.
        file: Option<PathBuf>,
        /// Cost cap for this check, in cents. The request that crosses the
        /// cap still finishes. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
        /// Parent directory for this check. A new subdirectory holds
        /// `trace.jsonl` and `context.md`. Defaults to `/tmp/autoground`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Compare the names, senses, and statements of two models.
    ///
    /// Each argument is a `.brief` file, a Markdown file of `brief` blocks,
    /// a `.mmd` flowchart, or brief source. A flowchart's node labels are
    /// names. An edge labeled with a sense name is that sense applied to its
    /// endpoints, and a leading `not ` negates it. Any other edge label is
    /// the wording between the two names. A `%%` line that starts with `(`
    /// is brief source, which is how a picture carries a sense template.
    /// Exit status is 1 when a name, a sense, or a statement is on only one
    /// side. The oracle is not asked.
    ///
    /// Examples:
    ///   autoground align model.brief picture.mmd
    ///   autoground align model.brief notes.md
    Align {
        /// A `.brief` file, Markdown `brief` blocks, a `.mmd` flowchart, or brief source.
        left: String,
        /// A `.brief` file, Markdown `brief` blocks, a `.mmd` flowchart, or brief source.
        right: String,
    },
    Sweep(SweepArgs),
}

/// Judge a brief against the filesystem in the current directory.
///
/// The brief has no `claim`, `witness`, `assume`, or `detail`. `--filter` keeps paths that
/// match it, from the listing alone, before any file is read. Each
/// remaining file is judged until `--budget` cents are spent. A file
/// matches when any top-level claim is true. Stdout is `CLEAN`, or the
/// absolute path of each match. A file whose request fails is reported
/// with `Error:` and the sweep continues. On a terminal the report
/// updates as files finish. A run that hits the cap ends with
/// `INCOMPLETE`. The trace is `/tmp/autoground/sweep-<id>.jsonl`.
///
/// `sweep transcript` judges a session. `sweep diff` judges each change
/// in `git diff`. Arguments after that brief are passed to `git diff`.
///
/// Examples:
///   autoground sweep examples/no-historic-comment.brief --filter "source file"
///   autoground sweep transcript examples/no-historic-comment.brief chat_history.jsonl
///   autoground sweep diff examples/no-historic-comment.brief
///   autoground sweep diff examples/no-historic-comment.brief --cached
///   autoground sweep diff examples/no-historic-comment.brief HEAD~3..HEAD
#[derive(Parser, Debug)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    subcommand_precedence_over_arg = true
)]
struct SweepArgs {
    #[command(subcommand)]
    universe: Option<SweepUniverse>,
    /// Brief source, a `.brief` file, or a Markdown file of `brief` blocks.
    #[arg(required = true, value_name = "BRIEF")]
    input: Option<String>,
    /// Natural-language filter for which paths to judge. Listing only.
    #[arg(long)]
    filter: Option<String>,
    /// Cost cap for this sweep, in cents. The request that crosses the
    /// cap still finishes. Later requests are refused.
    #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
    budget: u64,
}

#[derive(Subcommand, Debug)]
enum SweepUniverse {
    /// Judge the brief against a session transcript.
    ///
    /// `path` is `chat_history.jsonl`, or a directory that contains it.
    /// A match prints the database id.
    ///
    /// Example:
    ///   autoground sweep transcript examples/no-historic-comment.brief chat_history.jsonl
    Transcript {
        /// Model source, a `.brief` file, or a Markdown file of `brief` blocks.
        input: String,
        /// Session directory or `chat_history.jsonl`.
        path: PathBuf,
        /// Cost cap for this sweep, in cents. The request that crosses the
        /// cap still finishes. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
    },
    /// Judge the brief against each change in `git diff`.
    ///
    /// Arguments after the brief are passed to `git diff`. With no arguments
    /// this is the worktree against the index. A match prints the path of
    /// the change.
    ///
    /// Examples:
    ///   autoground sweep diff examples/no-historic-comment.brief
    ///   autoground sweep diff examples/no-historic-comment.brief --cached
    ///   autoground sweep diff examples/no-historic-comment.brief HEAD~3..HEAD
    ///   autoground sweep diff examples/no-historic-comment.brief origin/main...HEAD -- src/
    Diff {
        /// Model source, a `.brief` file, or a Markdown file of `brief` blocks.
        input: String,
        /// Cost cap for this sweep, in cents. The request that crosses the
        /// cap still finishes. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
        /// Arguments forwarded to `git diff`.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        git_args: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check {
            input,
            file,
            budget,
            out,
        } => match check_cmd(&input, file.as_deref(), budget, out.as_deref()) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Align { left, right } => match align_cmd(&left, &right) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Sweep(args) => match sweep_from(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
    }
}

fn sweep_from(args: &SweepArgs) -> Result<u8, Box<dyn std::error::Error>> {
    match &args.universe {
        None => {
            let Some(input) = args.input.as_deref() else {
                return Err("a brief is required".into());
            };
            let model = Path::new(input);
            let hide = model.is_file().then_some(model);
            sweep_cmd(
                input,
                args.budget,
                args.filter.as_deref(),
                sweep::Scope::Files {
                    root: Path::new("."),
                    hide,
                },
                Traced::Files,
            )
        }
        Some(SweepUniverse::Transcript {
            input,
            path,
            budget,
        }) => sweep_cmd(
            input,
            *budget,
            None,
            sweep::Scope::Transcript(path),
            Traced::Transcript(path),
        ),
        Some(SweepUniverse::Diff {
            input,
            budget,
            git_args,
        }) => {
            let model = Path::new(input);
            let hide = model.is_file().then_some(model);
            sweep_cmd(
                input,
                *budget,
                None,
                sweep::Scope::Diff {
                    root: Path::new("."),
                    git_args,
                    hide,
                },
                Traced::Diff(git_args),
            )
        }
    }
}

enum Traced<'a> {
    Files,
    Transcript(&'a Path),
    Diff(&'a [String]),
}

fn sweep_cmd(
    input: &str,
    budget_cents: u64,
    filter: Option<&str>,
    scope: sweep::Scope<'_>,
    traced: Traced<'_>,
) -> Result<u8, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let base = brief_dir(input);
    let (_src, _program, compiled) = load_compiled(input, &base)?;
    if !check::cites_dollar(&compiled.claims) {
        return Err("a sweep brief needs `(witness $)`".into());
    }
    let log = Log::create_sweep()?;
    let mut oracle = JevOracle::from_env(log.clone())?;
    oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);
    let mut live = if std::io::stdout().is_terminal() {
        Some(autoground::report::sweep_view::Live::open()?)
    } else {
        None
    };
    let outcome = sweep::sweep_watching(
        &compiled.claims,
        &base,
        scope,
        filter,
        &mut oracle,
        &mut |snapshot| {
            if let Some(live) = live.as_mut() {
                if let Some(status) = snapshot.status.as_deref() {
                    let _ = live.show(&snapshot.report, status);
                }
            }
        },
    )
    .map_err(|err| format!("{err}\ntrace: {}", log.path().display()))?;
    let matches: Vec<_> = outcome
        .matches
        .iter()
        .map(|matched| {
            json!({
                "path": matched.path.display().to_string(),
                "claims": matched.claims,
            })
        })
        .collect();
    let errors: Vec<_> = outcome
        .errors
        .iter()
        .map(|error| {
            json!({
                "path": error.path.display().to_string(),
                "error": error.message,
            })
        })
        .collect();
    let mut event = json!({
        "filter": filter,
        "incomplete": outcome.incomplete,
        "judged": outcome.judged,
        "candidates": outcome.candidates,
        "matches": matches,
        "errors": errors,
        "usage": UsageRecord::from_usage(&oracle.usage()),
    });
    match traced {
        Traced::Files => event["universe"] = json!("files"),
        Traced::Transcript(path) => {
            event["universe"] = json!("transcript");
            event["transcript"] = json!(path.display().to_string());
        }
        Traced::Diff(args) => {
            event["universe"] = json!("diff");
            event["git_diff"] = json!(args);
        }
    }
    log.append_event("sweep", &event)?;
    if let Some(live) = live.as_mut() {
        live.finish(&outcome.text)?;
    } else {
        print!("{}", outcome.text);
    }
    drop(live);
    let mut notes = Vec::new();
    if !outcome.errors.is_empty() {
        notes.push(sweep::failed_message(outcome.errors.len()));
    }
    if outcome.incomplete {
        notes.push(sweep::stopped_message(
            budget_cents as f64 / 100.0,
            oracle.usage().spend_usd,
            outcome.judged,
            outcome.candidates,
        ));
    }
    let note_refs: Vec<&str> = notes.iter().map(String::as_str).collect();
    print_sweep_summary(
        &oracle.usage(),
        oracle.flights(),
        oracle.flight_limit(),
        outcome.judged,
        outcome.candidates,
        started.elapsed(),
        log.path(),
        &note_refs,
    )?;
    Ok(
        if outcome.incomplete || !outcome.matches.is_empty() || !outcome.errors.is_empty() {
            1
        } else {
            0
        },
    )
}

fn check_cmd(
    input: &str,
    file: Option<&Path>,
    budget_cents: u64,
    out: Option<&Path>,
) -> Result<u8, Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    let live = if std::io::stdout().is_terminal() {
        let live = autoground::report::run::Live::open()?;
        live.show_checking()?;
        Some(live)
    } else {
        None
    };
    let base = brief_dir(input);
    let (src, program, compiled) = load_compiled(input, &base)?;
    let bound = file.map(|path| check::BoundFile {
        cite: path.display().to_string(),
        path: path.to_path_buf(),
    });
    match (check::cites_dollar(&compiled.claims), bound.is_some()) {
        (true, false) => return Err("`$` needs a file".into()),
        (false, true) => return Err("a file needs `(witness $)`".into()),
        _ => {}
    }
    let log = Log::create_under(out.unwrap_or_else(|| Path::new(DEFAULT_TRACE_DIR)))?;
    if let Some(live) = &live {
        let ui = live.clone();
        log.set_hook(move |event| ui.on_event(event));
    }
    let mut oracle = JevOracle::from_env(log.clone())?;
    oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);
    finish_check(
        &src,
        &program,
        &compiled.claims,
        &compiled.assumptions,
        &base,
        bound.as_ref(),
        &mut oracle,
        &log,
        live,
        started,
    )
}

fn finish_check<O: Oracle>(
    src: &str,
    program: &autoground::Program,
    claims: &[Proposition],
    assumptions: &[String],
    base: &Path,
    bound: Option<&check::BoundFile>,
    oracle: &mut O,
    log: &Log,
    live: Option<autoground::report::run::Live>,
    started: std::time::Instant,
) -> Result<u8, Box<dyn std::error::Error>> {
    let report = check::check_bound(claims, base, DEFAULT_BODY_BYTES, bound, oracle)?;
    log.clear_hook();
    let usage = oracle.usage();
    let flights = oracle.flights().to_vec();
    let flight_limit = oracle.flight_limit();
    let recorded: Vec<_> = report
        .claims
        .iter()
        .zip(program.clauses.iter())
        .map(|(node, clause)| {
            let claim = src
                .get(clause.span.clone())
                .map(str::trim)
                .unwrap_or(node.text.as_str());
            json!({
                "claim": claim,
                "value": node.value,
            })
        })
        .collect();
    log.append_event(
        "check",
        &json!({
            "claims": recorded,
            "usage": UsageRecord::from_usage(&usage),
        }),
    )?;
    write_context(log.path(), &report)?;
    if let Some(live) = live {
        live.finish(src, &program.clauses, &report)?;
    } else {
        print!("{}", write_report(&report));
    }
    let true_count = report
        .claims
        .iter()
        .filter(|claim| claim.value == autoground::Value::True)
        .count();
    print_run_summary(
        &usage,
        &flights,
        flight_limit,
        true_count,
        report.claims.len(),
        started.elapsed(),
        log.path(),
        assumptions,
    )?;
    let discharged = report
        .claims
        .iter()
        .all(|claim| claim.value == autoground::Value::True);
    Ok(if discharged { 0 } else { 1 })
}

/// Directory witness paths are relative to. A brief file uses its own directory.
/// Source passed on the command line uses the current directory.
fn brief_dir(input: &str) -> PathBuf {
    let path = Path::new(input);
    if path.is_file() {
        match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        }
    } else {
        PathBuf::from(".")
    }
}

fn align_cmd(left: &str, right: &str) -> Result<u8, Box<dyn std::error::Error>> {
    let left = load_model(left)?;
    let right = load_model(right)?;
    match mismatch(&left, &right) {
        None => Ok(0),
        Some(report) => {
            print!("{report}");
            Ok(1)
        }
    }
}

fn load_model(input: &str) -> Result<autoground::align::Model, Box<dyn std::error::Error>> {
    let src = align_source(input)?;
    let program = parse(&src)?;
    let base = brief_dir(input);
    let (compiled, _) = compile_linked(&program, input, &base)?;
    Ok(model_of(&program, &compiled))
}

/// `.mmd` lowers to a brief. Every other model is read the way `check` reads it.
fn align_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if has_ext(path, "mmd") {
        if !path.is_file() {
            return Err(format!("model file not found: {input}").into());
        }
        let text = std::fs::read_to_string(path)?;
        return match lower_mermaid(&text) {
            Ok(brief) => Ok(brief),
            Err(err) => Err(format!("{err} in {input}").into()),
        };
    }
    model_source(input)
}

fn load_compiled(
    input: &str,
    base: &Path,
) -> Result<(String, autoground::Program, autoground::Compiled), Box<dyn std::error::Error>> {
    let src = model_source(input)?;
    let program = parse(&src)?;
    let (compiled, _) = compile_linked(&program, input, base)?;
    Ok((src, program, compiled))
}

fn compile_linked(
    program: &autoground::Program,
    input: &str,
    base: &Path,
) -> Result<(autoground::Compiled, Vocabulary), Box<dyn std::error::Error>> {
    if program
        .namespace
        .as_ref()
        .is_some_and(|ns| !ns.requires.is_empty())
    {
        let files = scan_namespaces(base)?;
        let entry = Path::new(input);
        let entry_path = entry.is_file().then(|| display_path(base, entry));
        Ok(link_parts(program, entry_path.as_deref(), &files)?)
    } else {
        Ok((compile(program)?, Vocabulary::none()))
    }
}

/// Read `input` as a model file when it names one, otherwise treat it as source.
///
/// A `.brief` file is the model. A Markdown file contributes every fenced `brief` block.
fn model_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if path.is_file() {
        let text = std::fs::read_to_string(path)?;
        if has_ext(path, "md") {
            return Ok(brief_source(&text).map_err(|err| format!("{err} in {input}"))?);
        }
        return Ok(text);
    }
    let trimmed = input.trim_start();
    if trimmed.starts_with('(') {
        return Ok(input.to_string());
    }
    if has_ext(path, "brief") || has_ext(path, "md") || path.exists() {
        return Err(format!("model file not found: {input}").into());
    }
    Ok(input.to_string())
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_commands_select_a_universe() {
        let files = Cli::try_parse_from([
            "autoground",
            "sweep",
            "--budget",
            "2",
            "model.brief",
            "--filter",
            "source file",
        ])
        .unwrap();
        match files.command {
            Commands::Sweep(args) => {
                assert!(args.universe.is_none());
                assert_eq!(args.input.as_deref(), Some("model.brief"));
                assert_eq!(args.filter.as_deref(), Some("source file"));
                assert_eq!(args.budget, 2);
            }
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }

        let transcript = Cli::try_parse_from([
            "autoground",
            "sweep",
            "transcript",
            "--budget",
            "3",
            "model.brief",
            "chat_history.jsonl",
        ])
        .unwrap();
        match transcript.command {
            Commands::Sweep(args) => match args.universe {
                Some(SweepUniverse::Transcript {
                    input,
                    path,
                    budget,
                }) => {
                    assert_eq!(input, "model.brief");
                    assert_eq!(path, PathBuf::from("chat_history.jsonl"));
                    assert_eq!(budget, 3);
                }
                other => panic!("transcript parse: {other:?}"),
            },
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }

        let diff = Cli::try_parse_from([
            "autoground",
            "sweep",
            "diff",
            "--budget",
            "5",
            "model.brief",
            "--cached",
            "-U0",
            "origin/main...HEAD",
            "--",
            "src",
        ])
        .unwrap();
        match diff.command {
            Commands::Sweep(args) => match args.universe {
                Some(SweepUniverse::Diff {
                    input,
                    budget,
                    git_args,
                }) => {
                    assert_eq!(input, "model.brief");
                    assert_eq!(budget, 5);
                    assert_eq!(
                        git_args,
                        vec!["--cached", "-U0", "origin/main...HEAD", "--", "src",]
                    );
                }
                other => panic!("diff parse: {other:?}"),
            },
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }

        let late = Cli::try_parse_from([
            "autoground",
            "sweep",
            "diff",
            "model.brief",
            "--budget",
            "5",
            "--cached",
        ])
        .unwrap();
        match late.command {
            Commands::Sweep(args) => match args.universe {
                Some(SweepUniverse::Diff {
                    budget, git_args, ..
                }) => {
                    assert_eq!(budget, 5, "{git_args:?}");
                    assert_eq!(git_args, vec!["--cached"]);
                }
                other => panic!("late budget: {other:?}"),
            },
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }
    }

    #[test]
    fn a_filter_belongs_to_the_filesystem_sweep() {
        let err = Cli::try_parse_from([
            "autoground",
            "sweep",
            "--filter",
            "source",
            "diff",
            "model.brief",
        ])
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("filter") || text.contains("conflict"),
            "{text}"
        );
    }
}
