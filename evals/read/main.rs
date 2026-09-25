mod metrics;
mod plot;
mod price;
mod record;
mod trace;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use autoground::log::Log;
use autoground::oracle::{HeuristicOracle, JevOracle, Oracle};
use autoground::read::Reader;
use autoground::report::{format_duration, format_usd};
use autoground::search::DEFAULT_BUDGET_BYTES;
use autoground::universe::{Filesystem, Universe};
use autoground::Value;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::Value as Json;

use price::Prices;
use record::{Baseline, CaseRecord, HistoryRun, SuiteSummary};

#[derive(Parser)]
#[command(
    name = "read-eval",
    about = "Score read against a frozen per-proposition agent judgement"
)]
struct Cli {
    /// Repository to read. Defaults to the current directory.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ask Grok once per proposition and store its judgement.
    ///
    /// An id that already has a baseline is left as it is.
    Baseline {
        /// Replace stored baselines.
        #[arg(long)]
        refresh: bool,
        /// Run one proposition id.
        #[arg(long)]
        only: Option<String>,
    },
    /// Read each proposition alone, then all of them in one wait.
    Score {
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        /// Read only this repo-relative directory. `src` leaves out `evals/`.
        #[arg(long)]
        within: Option<String>,
    },
    /// Redraw `pareto.svg` from `history.jsonl`.
    Plot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum JudgeKind {
    /// TypeSafe Jev via TYPESAFE_API_KEY.
    Jev,
    /// Local stand-in. No API key. Does not append history.
    Heuristic,
}

impl JudgeKind {
    fn as_str(self) -> &'static str {
        match self {
            JudgeKind::Jev => "jev",
            JudgeKind::Heuristic => "heuristic",
        }
    }
}

#[derive(Deserialize)]
struct Proposition {
    id: String,
    proposition: String,
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let repo = cli.repo.canonicalize()?;
    let dir = repo.join("evals/read");
    match cli.command {
        Commands::Baseline { refresh, only } => baseline(&repo, &dir, refresh, only.as_deref()),
        Commands::Score { judge, within } => score(&repo, &dir, judge, within.as_deref()),
        Commands::Plot => redraw(&dir),
    }
}

fn baseline(
    repo: &Path,
    dir: &Path,
    refresh: bool,
    only: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let propositions = load_propositions(&propositions_path(repo))?;
    let selected = select(&propositions, only)?;
    let template = fs::read_to_string(dir.join("prompt.txt"))?;
    if !template.contains("{proposition}") {
        return Err("prompt.txt is missing {proposition}".into());
    }
    let schema = fs::read_to_string(dir.join("judgement.schema.json"))?;
    serde_json::from_str::<Json>(&schema).map_err(|err| format!("judgement.schema.json: {err}"))?;
    let prices = Prices::load(&prices_path(repo))?;
    let universe = Filesystem::at_root(repo)?;
    let leaves = trace::universe_leaves(&universe)?;
    let baseline_dir = dir.join("baseline");
    fs::create_dir_all(&baseline_dir)?;
    let mut failed = 0usize;
    for item in selected {
        let path = baseline_dir.join(format!("{}.json", item.id));
        if path.exists() && !refresh {
            eprintln!("{}  baseline exists", item.id);
            continue;
        }
        eprintln!("{}  asking Grok", item.id);
        match one_baseline(repo, &template, &schema, &prices, &leaves, item) {
            Ok(stored) => {
                write_json(&path, &stored)?;
                let cost = stored
                    .estimated_usd
                    .map(format_usd)
                    .unwrap_or_else(|| "n/a".to_string());
                let partial = if stored.estimate_partial {
                    " partial"
                } else {
                    ""
                };
                eprintln!(
                    "{}  {}  {} provenance  {cost}{partial}  {}  session {}",
                    stored.id,
                    stored.value,
                    stored.provenance.len(),
                    format_duration(Duration::from_millis(stored.duration_ms)),
                    stored.session_id
                );
            }
            Err(err) => {
                failed += 1;
                eprintln!("{}  {err}", item.id);
            }
        }
    }
    if failed > 0 {
        return Err(format!("{failed} baseline runs failed").into());
    }
    Ok(())
}

fn one_baseline(
    repo: &Path,
    template: &str,
    schema: &str,
    prices: &Prices,
    leaves: &HashSet<String>,
    item: &Proposition,
) -> Result<Baseline, String> {
    let prompt = template.replace("{proposition}", &item.proposition);
    let (headless, duration) = run_grok(repo, &prompt, schema)?;
    if headless.get("type").and_then(Json::as_str) == Some("error") {
        let message = headless
            .get("message")
            .and_then(Json::as_str)
            .unwrap_or("grok returned an error");
        return Err(message.to_string());
    }
    let session_id = session_id(&headless)?;
    let updates = fs::read_to_string(updates_path(&session_id)?)
        .map_err(|err| format!("session {session_id}: {err}"))?;
    let files = trace::files_from_updates(&updates, repo)?;
    let opened: HashSet<String> = files.reads.into_iter().collect();
    let judgement = trace::judgement_from_value(&headless)?;
    let (value, provenance) = trace::accept_judgement(&judgement, repo, leaves, &opened)?;
    let usage = price::usage_from_headless(&headless);
    let model = price::model_from_headless(&headless);
    let estimate = prices.estimate(model.as_deref(), &usage);
    Ok(Baseline {
        id: item.id.clone(),
        proposition: item.proposition.clone(),
        model,
        session_id,
        value,
        provenance,
        usage,
        estimated_usd: round_usd(estimate.usd),
        estimate_partial: estimate.partial,
        server_cost_usd: round_usd(price::server_cost_from_headless(&headless)),
        duration_ms: duration_ms(duration),
        priced_at: prices.priced_at.clone(),
        rate_band: prices.rate_band.clone(),
        price_source: prices.source.clone(),
    })
}

fn score(
    repo: &Path,
    dir: &Path,
    judge: JudgeKind,
    within: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let propositions = load_propositions(&propositions_path(repo))?;
    let baselines = load_baselines(dir, &propositions)?;
    if judge == JudgeKind::Jev {
        JevOracle::from_env().map_err(|err| err.to_string())?;
    }
    let each = pass(repo, &propositions, &baselines, judge, false, within)?;
    let batch = pass(repo, &propositions, &baselines, judge, true, within)?;
    let failed = each.failed || batch.failed;
    if judge == JudgeKind::Jev {
        let history_path = dir.join("history.jsonl");
        let mut runs = load_history(&history_path)?;
        let (git_sha, git_dirty) = git_state(repo);
        let (estimated, partial) = baseline_cost(&baselines);
        let agent = baseline_duration(&baselines);
        for scored in [&each, &batch] {
            runs.push(history_run(
                &git_sha, git_dirty, judge, estimated, partial, agent, scored,
            ));
        }
        write_history(&history_path, &runs)?;
        fs::write(dir.join("pareto.svg"), plot::render(&runs))?;
        eprintln!("history {}", history_path.display());
    }
    if failed {
        return Err("one or more reads failed".into());
    }
    Ok(())
}

struct Scored {
    batched: bool,
    cases: Vec<CaseRecord>,
    suite: SuiteSummary,
    read_spend: f64,
    requests: u64,
    elapsed: Duration,
    failed: bool,
}

fn pass(
    repo: &Path,
    propositions: &[Proposition],
    baselines: &BTreeMap<String, Baseline>,
    judge: JudgeKind,
    batched: bool,
    within: Option<&str>,
) -> Result<Scored, Box<dyn std::error::Error>> {
    let started = Instant::now();
    eprintln!("{}", if batched { "batch" } else { "each" });
    let mut cases = Vec::new();
    let mut read_spend = 0.0;
    let mut requests = 0u64;
    let mut failed = false;
    if batched {
        match read_batch(repo, propositions, judge, within) {
            Ok(batch) => {
                read_spend = batch.spend;
                requests = batch.requests;
                for (item, outcome) in propositions.iter().zip(batch.outcomes) {
                    cases.push(record_outcome(
                        item,
                        &baselines[&item.id],
                        outcome,
                        None,
                        &mut failed,
                    ));
                }
            }
            Err(failure) => {
                read_spend = failure.spend;
                requests = failure.requests;
                failed = true;
                for item in propositions {
                    eprintln!("{}  failed  {}", item.id, failure.message);
                    cases.push(failed_case(&item.id, None, failure.message.clone()));
                }
            }
        }
    } else {
        for item in propositions {
            eprintln!("{}  reading", item.id);
            let outcome = read_one(repo, &item.proposition, judge, within);
            let (spend, outcome_requests) = match &outcome {
                Ok(ran) => (ran.spend, ran.requests),
                Err(failure) => (failure.spend, failure.requests),
            };
            read_spend += spend;
            requests += outcome_requests;
            cases.push(record_outcome(
                item,
                &baselines[&item.id],
                outcome,
                Some(spend),
                &mut failed,
            ));
        }
    }
    let suite = suite_summary(&cases);
    let scored = Scored {
        batched,
        cases,
        suite,
        read_spend,
        requests,
        elapsed: started.elapsed(),
        failed,
    };
    print_recap(&scored, baselines);
    Ok(scored)
}

fn failed_case(id: &str, spend: Option<f64>, message: String) -> CaseRecord {
    CaseRecord {
        id: id.to_string(),
        status: "failed".to_string(),
        message: Some(message),
        read_spend_usd: spend,
        baseline_value: None,
        reader_value: None,
        matched: None,
        baseline_paths: Vec::new(),
        reader_evidence: Vec::new(),
    }
}

fn record_outcome(
    item: &Proposition,
    baseline: &Baseline,
    outcome: Result<Ran, ReadFailure>,
    spend: Option<f64>,
    failed: &mut bool,
) -> CaseRecord {
    match outcome {
        Ok(ran) => {
            let matched = baseline.value == ran.value;
            print_case(
                &item.id,
                baseline.value,
                ran.value,
                &paths_of(&baseline.provenance),
                &ran.evidence,
                spend,
                &ran.trace,
            );
            CaseRecord {
                id: item.id.clone(),
                status: "scored".to_string(),
                message: None,
                read_spend_usd: spend,
                baseline_value: Some(baseline.value),
                reader_value: Some(ran.value),
                matched: Some(matched),
                baseline_paths: paths_of(&baseline.provenance),
                reader_evidence: ran.evidence,
            }
        }
        Err(failure) => {
            *failed = true;
            eprintln!("{}  failed  {}", item.id, failure.message);
            failed_case(&item.id, spend, failure.message)
        }
    }
}

fn paths_of(provenance: &[record::Provenance]) -> Vec<String> {
    let mut paths: Vec<String> = provenance.iter().map(|item| item.path.clone()).collect();
    paths.sort();
    paths.dedup();
    paths
}

fn history_run(
    git_sha: &str,
    git_dirty: bool,
    judge: JudgeKind,
    estimated: Option<f64>,
    partial: bool,
    agent: Duration,
    scored: &Scored,
) -> HistoryRun {
    HistoryRun {
        git_sha: git_sha.to_string(),
        git_dirty,
        scored_at_ms: now_ms(),
        judge: judge.as_str().to_string(),
        batched: scored.batched,
        read_spend_usd: scored.read_spend,
        read_duration_ms: duration_ms(scored.elapsed),
        baseline_estimated_usd: estimated,
        baseline_estimate_partial: partial,
        baseline_duration_ms: duration_ms(agent),
        suite: scored.suite.clone(),
        cases: scored.cases.clone(),
    }
}

struct Ran {
    value: Value,
    evidence: Vec<String>,
    spend: f64,
    requests: u64,
    trace: PathBuf,
}

struct ReadFailure {
    spend: f64,
    requests: u64,
    message: String,
}

struct Batch {
    spend: f64,
    requests: u64,
    outcomes: Vec<Result<Ran, ReadFailure>>,
}

fn read_one(
    repo: &Path,
    proposition: &str,
    judge: JudgeKind,
    within: Option<&str>,
) -> Result<Ran, ReadFailure> {
    let universe = match open_universe(repo, within) {
        Ok(universe) => universe,
        Err(err) => return Err(err),
    };
    let log = match Log::create() {
        Ok(log) => log,
        Err(err) => {
            return Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: err.to_string(),
            })
        }
    };
    let trace = log.path().to_path_buf();
    let result = match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            wait(universe, oracle, proposition, &log)
        }
        JudgeKind::Jev => match JevOracle::from_env() {
            Ok(oracle) => wait(universe, oracle, proposition, &log),
            Err(err) => Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: err.to_string(),
            }),
        },
    };
    match result {
        Ok(ran) => Ok(Ran { trace, ..ran }),
        Err(failure) => Err(ReadFailure {
            message: format!("{}  trace {}", failure.message, trace.display()),
            ..failure
        }),
    }
}

fn read_batch(
    repo: &Path,
    propositions: &[Proposition],
    judge: JudgeKind,
    within: Option<&str>,
) -> Result<Batch, ReadFailure> {
    let universe = match open_universe(repo, within) {
        Ok(universe) => universe,
        Err(err) => return Err(err),
    };
    let log = match Log::create() {
        Ok(log) => log,
        Err(err) => {
            return Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: err.to_string(),
            })
        }
    };
    let trace = log.path().to_path_buf();
    let result = match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            wait_all(universe, oracle, propositions, &log)
        }
        JudgeKind::Jev => match JevOracle::from_env() {
            Ok(oracle) => wait_all(universe, oracle, propositions, &log),
            Err(err) => Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: err.to_string(),
            }),
        },
    };
    match result {
        Ok(batch) => Ok(Batch {
            outcomes: batch
                .outcomes
                .into_iter()
                .map(|outcome| attach_trace(outcome, &trace))
                .collect(),
            ..batch
        }),
        Err(failure) => Err(ReadFailure {
            message: format!("{}  trace {}", failure.message, trace.display()),
            ..failure
        }),
    }
}

fn attach_trace(outcome: Result<Ran, ReadFailure>, trace: &Path) -> Result<Ran, ReadFailure> {
    match outcome {
        Ok(ran) => Ok(Ran {
            trace: trace.to_path_buf(),
            ..ran
        }),
        Err(failure) => Err(ReadFailure {
            message: format!("{}  trace {}", failure.message, trace.display()),
            ..failure
        }),
    }
}

fn wait_all<O: Oracle>(
    universe: Filesystem,
    oracle: O,
    propositions: &[Proposition],
    log: &Log,
) -> Result<Batch, ReadFailure> {
    let mut reader = Reader::new(universe, oracle, DEFAULT_BUDGET_BYTES);
    reader.set_log(log.clone());
    let tickets: Vec<_> = propositions
        .iter()
        .map(|item| {
            eprintln!("{}  reading", item.id);
            reader.submit(item.proposition.clone())
        })
        .collect();
    let findings = reader.wait_many(&tickets);
    let usage = reader.usage();
    let outcomes = findings
        .into_iter()
        .map(|finding| match finding {
            Ok(finding) => Ok(ran_from_finding(finding)),
            Err(err) => Err(ReadFailure {
                spend: usage.spend_usd,
                requests: err.usage.requests,
                message: err.to_string(),
            }),
        })
        .collect();
    Ok(Batch {
        spend: usage.spend_usd,
        requests: usage.requests,
        outcomes,
    })
}

fn wait<O: Oracle>(
    universe: Filesystem,
    oracle: O,
    proposition: &str,
    log: &Log,
) -> Result<Ran, ReadFailure> {
    let mut reader = Reader::new(universe, oracle, DEFAULT_BUDGET_BYTES);
    reader.set_log(log.clone());
    let ticket = reader.submit(proposition);
    match reader.wait(ticket) {
        Ok(finding) => Ok(ran_from_finding(finding)),
        Err(err) => Err(ReadFailure {
            spend: err.usage.spend_usd,
            requests: err.usage.requests,
            message: err.to_string(),
        }),
    }
}

fn open_universe(repo: &Path, within: Option<&str>) -> Result<Filesystem, ReadFailure> {
    let mut universe = Filesystem::at_root(repo).map_err(|err| ReadFailure {
        spend: 0.0,
        requests: 0,
        message: err.to_string(),
    })?;
    if let Some(prefix) = within {
        if prefix.is_empty()
            || prefix.starts_with('/')
            || prefix
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: format!("{prefix} is not a repo-relative directory"),
            });
        }
        if !repo.join(prefix).is_dir() {
            return Err(ReadFailure {
                spend: 0.0,
                requests: 0,
                message: format!("{prefix} is not a directory"),
            });
        }
        universe.set_only(prefix);
    }
    Ok(universe)
}

fn ran_from_finding(finding: autoground::read::Finding<Filesystem>) -> Ran {
    let mut evidence = finding.evidence.databases().to_vec();
    evidence.sort();
    Ran {
        value: finding.value,
        evidence,
        spend: finding.usage.spend_usd,
        requests: finding.usage.requests,
        trace: PathBuf::new(),
    }
}

fn print_case(
    id: &str,
    baseline: Value,
    reader: Value,
    baseline_paths: &[String],
    reader_evidence: &[String],
    spend: Option<f64>,
    trace: &Path,
) {
    let cost = match spend {
        Some(usd) => format!("  {}", format_usd(usd)),
        None => String::new(),
    };
    if baseline == reader {
        eprintln!("{id}  {reader}  match{cost}  trace {}", trace.display());
        return;
    }
    eprintln!(
        "{id}  reader {reader}  baseline {baseline}{cost}  trace {}",
        trace.display()
    );
    eprintln!("  reader");
    print_paths(reader_evidence);
    eprintln!("  baseline");
    print_paths(baseline_paths);
}

fn print_paths(paths: &[String]) {
    if paths.is_empty() {
        eprintln!("    (none)");
        return;
    }
    for path in paths {
        eprintln!("    {path}");
    }
}

fn print_recap(scored: &Scored, baselines: &BTreeMap<String, Baseline>) {
    let label = if scored.batched { "batch" } else { "each" };
    let (estimated, partial) = baseline_cost(baselines);
    let baseline = match estimated {
        Some(usd) => format_usd(usd),
        None => "n/a".to_string(),
    };
    let partial = if partial { " partial" } else { "" };
    let matched = scored
        .cases
        .iter()
        .filter(|case| case.matched == Some(true))
        .count();
    eprintln!(
        "{label:<5}  accuracy {} ({matched}/{})  scored {}  failed {}  read {} in {}  requests {}  baseline {baseline}{partial} in {}",
        fmt_pct(scored.suite.accuracy),
        scored.suite.scored,
        scored.suite.scored,
        scored.suite.failed,
        format_usd(scored.read_spend),
        format_duration(scored.elapsed),
        scored.requests,
        format_duration(baseline_duration(baselines)),
    );
}

fn suite_summary(cases: &[CaseRecord]) -> SuiteSummary {
    let rows: Vec<_> = cases
        .iter()
        .filter_map(|case| {
            case.baseline_value
                .map(|baseline| (baseline, case.reader_value))
        })
        .collect();
    SuiteSummary {
        accuracy: metrics::accuracy(&rows),
        scored: rows.len(),
        failed: cases.iter().filter(|case| case.status == "failed").count(),
    }
}

fn redraw(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let history_path = dir.join("history.jsonl");
    let runs = load_history(&history_path)?;
    fs::write(dir.join("pareto.svg"), plot::render(&runs))?;
    Ok(())
}

fn baseline_cost(baselines: &BTreeMap<String, Baseline>) -> (Option<f64>, bool) {
    let mut sum = 0.0;
    let mut any = false;
    let mut partial = false;
    for baseline in baselines.values() {
        match baseline.estimated_usd {
            Some(usd) => {
                sum += usd;
                any = true;
            }
            None => partial = true,
        }
        if baseline.estimate_partial {
            partial = true;
        }
    }
    (any.then_some(sum), partial)
}

fn baseline_duration(baselines: &BTreeMap<String, Baseline>) -> Duration {
    baselines.values().fold(Duration::ZERO, |sum, baseline| {
        sum + Duration::from_millis(baseline.duration_ms)
    })
}

fn load_baselines(
    dir: &Path,
    propositions: &[Proposition],
) -> Result<BTreeMap<String, Baseline>, Box<dyn std::error::Error>> {
    let mut missing = Vec::new();
    let mut baselines = BTreeMap::new();
    for item in propositions {
        let path = dir.join("baseline").join(format!("{}.json", item.id));
        if !path.exists() {
            missing.push(item.id.clone());
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let baseline: Baseline = serde_json::from_str(&text)?;
        if baseline.id != item.id {
            return Err(format!("{} id is {}", path.display(), baseline.id).into());
        }
        baselines.insert(item.id.clone(), baseline);
    }
    if !missing.is_empty() {
        return Err(format!(
            "missing baseline for {}. Run read-eval baseline",
            missing.join(", ")
        )
        .into());
    }
    Ok(baselines)
}

fn load_history(path: &Path) -> Result<Vec<HistoryRun>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut runs = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        runs.push(
            serde_json::from_str(line)
                .map_err(|err| format!("{}:{}: {err}", path.display(), number + 1))?,
        );
    }
    Ok(runs)
}

fn write_history(path: &Path, runs: &[HistoryRun]) -> Result<(), Box<dyn std::error::Error>> {
    let mut text = String::new();
    for run in runs {
        text.push_str(&serde_json::to_string(run)?);
        text.push('\n');
    }
    fs::write(path, text)?;
    Ok(())
}

fn load_propositions(path: &Path) -> Result<Vec<Proposition>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let items: Vec<Proposition> = serde_json::from_str(&text)?;
    let mut seen = HashSet::new();
    for item in &items {
        if !valid_id(&item.id) {
            return Err(format!("proposition id {} is not a file name", item.id).into());
        }
        if !seen.insert(item.id.clone()) {
            return Err(format!("duplicate proposition id {}", item.id).into());
        }
    }
    Ok(items)
}

fn select<'a>(
    items: &'a [Proposition],
    only: Option<&str>,
) -> Result<Vec<&'a Proposition>, Box<dyn std::error::Error>> {
    let Some(only) = only else {
        return Ok(items.iter().collect());
    };
    let selected: Vec<_> = items.iter().filter(|item| item.id == only).collect();
    if selected.is_empty() {
        return Err(format!("no proposition {only}").into());
    }
    Ok(selected)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn propositions_path(repo: &Path) -> PathBuf {
    repo.join("evals/search/propositions.json")
}

fn prices_path(repo: &Path) -> PathBuf {
    repo.join("evals/search/prices.toml")
}

fn run_grok(repo: &Path, prompt: &str, schema: &str) -> Result<(Json, Duration), String> {
    let prompt_path = std::env::temp_dir().join(format!("read-eval-prompt-{}.txt", now_ms()));
    fs::write(&prompt_path, prompt).map_err(|err| err.to_string())?;
    let started = Instant::now();
    let output = Command::new("grok")
        .current_dir(repo)
        .env("GROK_MEMORY", "0")
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .arg("--prompt-file")
        .arg(&prompt_path)
        .arg("--cwd")
        .arg(repo)
        .arg("--output-format")
        .arg("json")
        .arg("--json-schema")
        .arg(schema)
        .arg("--yolo")
        .arg("--no-subagents")
        .arg("--disable-web-search")
        .arg("--no-auto-update")
        .arg("--max-turns")
        .arg("40")
        .arg("--tools")
        .arg("read_file,grep,list_dir")
        .arg("--deny")
        .arg("Read(evals/**)")
        .arg("--deny")
        .arg("Grep(evals/**)")
        .output();
    let duration = started.elapsed();
    let _ = fs::remove_file(&prompt_path);
    let output = output.map_err(|err| format!("grok: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "grok exited {}\n{}",
            output.status,
            tail(&output.stderr)
        ));
    }
    Ok((parse_headless(&output.stdout)?, duration))
}

fn parse_headless(stdout: &[u8]) -> Result<Json, String> {
    let text = String::from_utf8_lossy(stdout);
    if let Ok(value) = serde_json::from_str::<Json>(text.trim()) {
        return Ok(value);
    }
    for line in text.lines().rev() {
        if let Ok(value) = serde_json::from_str::<Json>(line.trim()) {
            if value.get("sessionId").is_some()
                || value.get("type").and_then(Json::as_str) == Some("error")
            {
                return Ok(value);
            }
        }
    }
    Err(format!("grok stdout was not JSON\n{}", tail(stdout)))
}

fn session_id(value: &Json) -> Result<String, String> {
    let id = value
        .get("sessionId")
        .and_then(Json::as_str)
        .ok_or("headless JSON has no sessionId")?;
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("unexpected session id {id}"));
    }
    Ok(id.to_string())
}

fn updates_path(session_id: &str) -> Result<PathBuf, String> {
    let sessions = grok_home().join("sessions");
    let entries =
        fs::read_dir(&sessions).map_err(|err| format!("{}: {err}", sessions.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let candidate = entry.path().join(session_id).join("updates.jsonl");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "no updates.jsonl for session {session_id} under {}",
        sessions.display()
    ))
}

fn grok_home() -> PathBuf {
    match std::env::var("GROK_HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".grok")
        }
    }
}

fn git_state(repo: &Path) -> (String, bool) {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false);
    (sha, dirty)
}

fn write_json(
    path: &Path,
    value: &impl serde::Serialize,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn round_usd(value: Option<f64>) -> Option<f64> {
    value.map(|usd| (usd * 1_000_000.0).round() / 1_000_000.0)
}

fn fmt_pct(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{:.1}%", value * 100.0),
        None => "n/a".to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let start = text.len().saturating_sub(2000);
    text[start..].trim().to_string()
}
