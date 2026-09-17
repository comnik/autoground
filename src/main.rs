use std::path::{Path, PathBuf};
use std::process::ExitCode;

use autoground::edb::Store;
use autoground::eval::run;
use autoground::navigator::{
    Arg, Demand, FixtureJudge, JevJudge, Navigator, Need, RecordingJudge, TreeNavigator,
};
use autoground::report::{format_run, run_exit_code};
use autoground::syntax::Term;
use autoground::{compile, parse, parse_atom, verbalize_source};
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
    /// Local stand-in (default). No API key.
    Heuristic,
    /// TypeSafe Jev via TYPESAFE_API_KEY.
    Jev,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse a model and evaluate its requires against EDB, optionally grounding
    /// from a repository via the navigator.
    Run {
        /// Path to a `.ag` model.
        model: PathBuf,
        /// Repository to ground against.
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = JudgeKind::Heuristic)]
        judge: JudgeKind,
        #[arg(long)]
        fixtures: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Ground a single atom and step through the walk.
    ///
    /// Examples:
    ///   autoground ground --repo examples/toy-split '(component "Frontend")'
    ///   autoground ground --repo examples/toy-split --model examples/layered.ag '(component "Frontend")'
    Ground {
        /// Atom to ground, e.g. `(component "Frontend")` or `(component ?x)`.
        atom: String,
        /// Repository to ground against.
        #[arg(long)]
        repo: PathBuf,
        /// Model whose `declare` forms verbalize the atom for Jev.
        #[arg(long)]
        model: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = JudgeKind::Heuristic)]
        judge: JudgeKind,
        #[arg(long)]
        fixtures: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
        /// Print every step instead of the interactive pager.
        #[arg(long)]
        plain: bool,
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
            record,
        } => match run_model(
            &model,
            repo.as_ref(),
            judge,
            fixtures.as_ref(),
            record.as_ref(),
        ) {
            Ok(code) => ExitCode::from(code as u8),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Ground {
            atom,
            repo,
            model,
            judge,
            fixtures,
            record,
            plain,
        } => match ground_atom(
            &atom,
            &repo,
            model.as_ref(),
            judge,
            fixtures.as_ref(),
            record.as_ref(),
            plain,
        ) {
            Ok(code) => ExitCode::from(code as u8),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
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
    let src = if Path::new(input).is_file() {
        std::fs::read_to_string(input)?
    } else {
        input.to_string()
    };
    for line in verbalize_source(&src)? {
        println!("{line}");
    }
    Ok(())
}

fn run_model(
    model: &PathBuf,
    repo: Option<&PathBuf>,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
    record: Option<&PathBuf>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let src = std::fs::read_to_string(model)?;
    let program = parse(&src)?;
    let compiled = compile(&program)?;
    let mut store = Store::new();
    let result = if let Some(repo) = repo {
        let mut nav = make_nav(repo, judge, fixtures, record)?;
        let result = run(&compiled, &mut store, Some(&mut nav));
        if !nav.errors.is_empty() {
            for e in &nav.errors {
                eprintln!("judge error: {e}");
            }
            print!("{}", format_run(&compiled, &store, &result));
            return Ok(3);
        }
        result
    } else {
        run(&compiled, &mut store, None)
    };
    print!("{}", format_run(&compiled, &store, &result));
    Ok(run_exit_code(&result.outcome))
}

fn ground_atom(
    atom: &str,
    repo: &Path,
    model: Option<&PathBuf>,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
    record: Option<&PathBuf>,
    plain: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    let (pred, terms) = parse_atom(atom)?;
    let args: Vec<Arg> = terms
        .iter()
        .map(|t| match t {
            Term::Const(c) | Term::Typed { value: c, .. } => Arg::Bound(c.clone()),
            Term::Var(v) => Arg::Free(v.clone()),
        })
        .collect();
    let demand = Demand {
        pred,
        args,
        need: Need::Truth,
    };
    let store = Store::new();
    let mut nav = make_nav(repo, judge, fixtures, record)?;
    if let Some(model) = model {
        let src = std::fs::read_to_string(model)?;
        let compiled = compile(&parse(&src)?)?;
        nav.prepare(&compiled);
    }
    let answer = nav.answer(&demand, &store);
    if !nav.errors.is_empty() {
        for e in &nav.errors {
            eprintln!("judge error: {e}");
        }
        return Ok(3);
    }
    let repo_name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    if plain {
        print!(
            "{}",
            autoground::report::step::format_trace(&nav.trace, repo_name)
        );
        if nav.trace.is_empty() {
            eprintln!("(no steps recorded; bindings: {:?})", answer.bindings);
        }
        return Ok(if answer.bindings.is_empty() { 1 } else { 0 });
    }
    autoground::report::tui::page_trace(&nav.trace, repo_name)?;
    Ok(if answer.bindings.is_empty() { 1 } else { 0 })
}

fn make_nav(
    repo: &Path,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
    record: Option<&PathBuf>,
) -> Result<TreeNavigator, Box<dyn std::error::Error>> {
    let inner: Box<dyn autoground::navigator::Judge> = if let Some(path) = fixtures {
        Box::new(FixtureJudge::load(path)?)
    } else {
        match judge {
            JudgeKind::Heuristic => Box::new(autoground::HeuristicJudge),
            JudgeKind::Jev => Box::new(JevJudge::from_env()?),
        }
    };
    let boxed: Box<dyn autoground::navigator::Judge> = if let Some(path) = record {
        Box::new(RecordingJudge::new(inner, path))
    } else {
        inner
    };
    Ok(TreeNavigator::new(repo, boxed))
}
