mod metrics;
mod render;
mod strategy;
mod suite;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use autoground::check::strategy::{Strategy, Verdict};
use autoground::log::Log;
use autoground::oracle::jev::DEFAULT_RUN_COST_CENTS;
use autoground::oracle::{Classification, JevOracle, Oracle, UsageRecord};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;

use crate::metrics::{compare, summarize, RunReport};
use crate::render::{render_comparison, render_run};
use crate::strategy::ConfigurableStrategy;
use crate::suite::{load_suites, Probe, ProbeKind, ProbeState};

#[derive(Parser, Debug)]
#[command(name = "qa", about = "Quality assurance utilities for autoground.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Evaluate oracle prompting and state strategies on probe suites.
    Oracle(OracleArgs),
}

#[derive(Parser, Debug)]
struct OracleArgs {
    /// Probe suite TOML file(s). Defaults to qa/suites/kernel.toml (and regressions.toml if present).
    #[arg(long = "suite", value_name = "PATH")]
    suites: Vec<PathBuf>,
    /// Strategy name or path to a strategy TOML overlay.
    #[arg(long, default_value = "default")]
    strategy: String,
    /// Optional baseline strategy for A/B comparison.
    #[arg(long)]
    baseline: Option<String>,
    /// Number of sequential passes over the suite.
    #[arg(long, default_value_t = 1)]
    repeats: usize,
    /// Total cost cap in cents across this invocation.
    #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
    budget: u64,
    /// Filter to probes matching any given tag.
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// Filter to one oracle question kind.
    #[arg(long, value_enum)]
    kind: Option<KindArg>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = FormatArg::Human)]
    format: FormatArg,
    /// Directory for trace logs.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Build requests and print summary + sample payloads without calling Jev.
    #[arg(long)]
    dry_run: bool,
    /// List built-in strategies and exit.
    #[arg(long)]
    list_strategies: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum KindArg {
    Document,
    Part,
    Joint,
    Implication,
}

impl From<KindArg> for ProbeKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Document => ProbeKind::Document,
            KindArg::Part => ProbeKind::Part,
            KindArg::Joint => ProbeKind::Joint,
            KindArg::Implication => ProbeKind::Implication,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FormatArg {
    Human,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.command {
        Commands::Oracle(args) => run_oracle(args),
    };
    match res {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run_oracle(args: OracleArgs) -> Result<u8, String> {
    if args.list_strategies {
        for (name, desc) in ConfigurableStrategy::builtins() {
            println!("{name:<24} {desc}");
        }
        return Ok(0);
    }

    let repo_root = find_repo_root()?;
    let suite_paths = resolve_suites(&args.suites, &repo_root)?;
    let mut probes = load_suites(&suite_paths, &repo_root)?;
    if let Some(kind) = args.kind {
        let want: ProbeKind = kind.into();
        probes.retain(|p| p.state.kind() == want);
    }
    if !args.tags.is_empty() {
        probes.retain(|p| args.tags.iter().any(|t| p.tags.contains(t)));
    }
    if probes.is_empty() {
        return Err("no probes matched the given suite/filters".into());
    }

    let candidate = ConfigurableStrategy::load(&args.strategy)?;
    let baseline = match &args.baseline {
        Some(spec) => Some(ConfigurableStrategy::load(spec)?),
        None => None,
    };

    if args.dry_run {
        if let Some(base) = &baseline {
            print_dry_run(base.as_ref(), &probes);
            println!();
        }
        print_dry_run(candidate.as_ref(), &probes);
        return Ok(0);
    }

    let repeats = args.repeats.max(1);
    let out_dir = args
        .out
        .unwrap_or_else(|| PathBuf::from("/tmp/autoground/qa"));
    let total_budget_usd = args.budget as f64 / 100.0;

    if let Some(base) = baseline {
        let base_report = execute_strategy(base.as_ref(), &probes, repeats, total_budget_usd, &out_dir)?;
        let remaining = (total_budget_usd - base_report.usage.spend_usd).max(0.0);
        let cand_report =
            execute_strategy(candidate.as_ref(), &probes, repeats, remaining, &out_dir)?;
        let comp = compare(base_report, cand_report);
        match args.format {
            FormatArg::Human => print!("{}", render_comparison(&comp)),
            FormatArg::Json => println!(
                "{}",
                serde_json::to_string_pretty(&comp).map_err(|e| e.to_string())?
            ),
        }
    } else {
        let report =
            execute_strategy(candidate.as_ref(), &probes, repeats, total_budget_usd, &out_dir)?;
        match args.format {
            FormatArg::Human => print!("{}", render_run(&report)),
            FormatArg::Json => println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            ),
        }
    }

    Ok(0)
}

fn execute_strategy(
    strategy: &dyn Strategy,
    probes: &[Probe],
    repeats: usize,
    budget_usd: f64,
    out_dir: &Path,
) -> Result<RunReport, String> {
    let started = Instant::now();
    let log = Log::create_under(out_dir).map_err(|e| e.to_string())?;
    let mut oracle = JevOracle::from_env(log.clone()).map_err(|e| e.to_string())?;
    oracle.set_run_cost_limit_usd(budget_usd);

    let mut verdicts_per_probe: Vec<Vec<Verdict>> = vec![Vec::with_capacity(repeats); probes.len()];

    for _ in 0..repeats {
        let mut ticket_spans = Vec::with_capacity(probes.len());
        let mut all_tickets = Vec::new();
        for probe in probes {
            let classes = build_classifications(strategy, probe);
            let start = all_tickets.len();
            for class in classes {
                all_tickets.push(oracle.submit(class));
            }
            ticket_spans.push(start..all_tickets.len());
        }
        let answers = oracle.wait_many(&all_tickets);
        for (idx, span) in ticket_spans.into_iter().enumerate() {
            let mut probe_answers = Vec::with_capacity(span.len());
            for res in &answers[span] {
                let ans = res.as_ref().map_err(|e| e.to_string())?;
                probe_answers.push(ans.clone());
            }
            verdicts_per_probe[idx].push(strategy.settle(&probe_answers));
        }
    }

    let usage = UsageRecord::from_usage(&oracle.usage());
    let trace_path = log.path().display().to_string();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(summarize(
        strategy.name().to_string(),
        repeats,
        probes,
        &verdicts_per_probe,
        usage,
        trace_path,
        elapsed_ms,
    ))
}

fn build_classifications(strategy: &dyn Strategy, probe: &Probe) -> Vec<Classification> {
    match &probe.state {
        ProbeState::Document { path, body } => strategy.document(&probe.statement, path, body, None),
        ProbeState::Part {
            path,
            body,
            part,
            parts,
        } => strategy.document(&probe.statement, path, body, Some((*part, *parts))),
        ProbeState::Joint { files } => strategy.joint(&probe.statement, files),
        ProbeState::Implication { premises } => strategy.implication(&probe.statement, premises),
    }
}

fn print_dry_run(strategy: &dyn Strategy, probes: &[Probe]) {
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_expected: BTreeMap<&str, usize> = BTreeMap::new();
    let mut state_keys: HashSet<String> = HashSet::new();
    let mut samples: BTreeMap<ProbeKind, (String, Classification)> = BTreeMap::new();
    let mut total_questions = 0usize;

    for probe in probes {
        *by_kind.entry(probe.state.kind().as_str()).or_insert(0) += 1;
        *by_expected.entry(probe.expected.as_str()).or_insert(0) += 1;
        let classes = build_classifications(strategy, probe);
        total_questions += classes.len();
        for class in &classes {
            state_keys.insert(class.state.to_string());
        }
        let kind = probe.state.kind();
        let prefer_sample = !samples.contains_key(&kind)
            || (!probe.statement.roles.is_empty()
                && samples
                    .get(&kind)
                    .is_some_and(|(id, _)| !id.contains("sense")));
        if prefer_sample {
            if let Some(first) = classes.into_iter().next() {
                samples.insert(kind, (probe.id.clone(), first));
            }
        }
    }

    println!(
        "dry-run strategy={}  probes={}  questions={}  unique_states={}",
        strategy.name(),
        probes.len(),
        total_questions,
        state_keys.len()
    );
    println!("  by_kind:     {by_kind:?}");
    println!("  by_expected: {by_expected:?}");
    for (kind, (probe_id, class)) in samples {
        let preview = json!({
            "type": class.kind.as_str(),
            "instructions": class.instructions,
            "criteria": class.criteria,
            "state_keys": class.state.as_object().map(|m| m.keys().collect::<Vec<_>>()),
        });
        println!(
            "\nsample [{} / {}]:\n{}",
            kind.as_str(),
            probe_id,
            serde_json::to_string_pretty(&preview).unwrap_or_default()
        );
    }
}

fn resolve_suites(given: &[PathBuf], repo_root: &Path) -> Result<Vec<PathBuf>, String> {
    if !given.is_empty() {
        return Ok(given.to_vec());
    }
    let mut defaults = Vec::new();
    let kernel = repo_root.join("qa/suites/kernel.toml");
    if kernel.is_file() {
        defaults.push(kernel);
    }
    let regressions = repo_root.join("qa/suites/regressions.toml");
    if regressions.is_file() {
        defaults.push(regressions);
    }
    if defaults.is_empty() {
        return Err("no default suites found under qa/suites/".into());
    }
    Ok(defaults)
}

fn find_repo_root() -> Result<PathBuf, String> {
    let mut dir = std::env::current_dir().map_err(|e| e.to_string())?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("src/check/mod.rs").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("could not locate autoground repository root".into());
        }
    }
}
