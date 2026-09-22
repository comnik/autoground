mod metrics;
mod plot;
mod price;
mod record;
mod trace;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use autoground::log::Log;
use autoground::navigator::confidence::SUPPORT;
use autoground::oracle::{HeuristicOracle, JevOracle, Oracle};
use autoground::report::format_usd;
use autoground::search::{Search, DEFAULT_BUDGET_BYTES};
use autoground::universe::{Filesystem, Universe, UniverseError};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::Value;

use metrics::FloorPoint;
use price::Prices;
use record::{Baseline, CaseRecord, CurvePoint, FloorRecord, HistoryRun, SuiteSummary};

const FLOORS: [f64; 5] = [0.50, 0.70, SUPPORT, 0.95, 1.00];

#[derive(Parser)]
#[command(
    name = "search-eval",
    about = "Score search against a frozen per-proposition agent baseline"
)]
struct Cli {
    /// Repository to search. Defaults to the current directory.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ask Grok once per proposition and store the files it opened.
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
    /// Run one search per proposition and compare its window to the baseline.
    Score {
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
    },
    /// Redraw `pareto.svg` from `history.jsonl`.
    ///
    /// Fills a missing evidence-only prune line from the stored baselines.
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
    let dir = repo.join("evals/search");
    match cli.command {
        Commands::Baseline { refresh, only } => baseline(&repo, &dir, refresh, only.as_deref()),
        Commands::Score { judge } => score(&repo, &dir, judge),
        Commands::Plot => redraw(&repo, &dir),
    }
}

fn baseline(
    repo: &Path,
    dir: &Path,
    refresh: bool,
    only: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let propositions = load_propositions(&dir.join("propositions.json"))?;
    let selected = select(&propositions, only)?;
    let template = fs::read_to_string(dir.join("prompt.txt"))?;
    if !template.contains("{proposition}") {
        return Err("prompt.txt is missing {proposition}".into());
    }
    let prices = Prices::load(&dir.join("prices.toml"))?;
    let universe = Filesystem::at_root(repo)?;
    let leaves = universe_leaves(&universe)?;
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
        match one_baseline(repo, &template, &prices, &leaves, item) {
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
                    "{}  {} evidence files  {cost}{partial}  session {}",
                    stored.id,
                    stored.evidence.len(),
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
    prices: &Prices,
    leaves: &HashSet<String>,
    item: &Proposition,
) -> Result<Baseline, String> {
    let prompt = template.replace("{proposition}", &item.proposition);
    let headless = run_grok(repo, &prompt)?;
    if headless.get("type").and_then(Value::as_str) == Some("error") {
        let message = headless
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("grok returned an error");
        return Err(message.to_string());
    }
    let session_id = session_id(&headless)?;
    let updates = fs::read_to_string(updates_path(&session_id)?)
        .map_err(|err| format!("session {session_id}: {err}"))?;
    let files = trace::files_from_updates(&updates, repo)?;
    let (evidence, not_leaves) = trace::partition_universe(&files.reads, leaves);
    let mut dropped = files.outside;
    dropped.extend(not_leaves);
    dropped.sort();
    dropped.dedup();
    let usage = price::usage_from_headless(&headless);
    let model = price::model_from_headless(&headless);
    let estimate = prices.estimate(model.as_deref(), &usage);
    Ok(Baseline {
        id: item.id.clone(),
        proposition: item.proposition.clone(),
        model,
        session_id,
        evidence,
        dropped,
        navigation: files.navigation,
        usage,
        estimated_usd: round_usd(estimate.usd),
        estimate_partial: estimate.partial,
        server_cost_usd: round_usd(price::server_cost_from_headless(&headless)),
        priced_at: prices.priced_at.clone(),
        rate_band: prices.rate_band.clone(),
        price_source: prices.source.clone(),
    })
}

fn score(repo: &Path, dir: &Path, judge: JudgeKind) -> Result<(), Box<dyn std::error::Error>> {
    let propositions = load_propositions(&dir.join("propositions.json"))?;
    let baselines = load_baselines(dir, &propositions)?;
    if judge == JudgeKind::Jev {
        JevOracle::from_env().map_err(|err| err.to_string())?;
    }
    let mut cases = Vec::new();
    let mut points_by_case: Vec<Vec<FloorPoint>> = Vec::new();
    let mut ideals = Vec::new();
    let mut search_spend = 0.0;
    for item in &propositions {
        let baseline = &baselines[&item.id];
        if baseline.evidence.is_empty() {
            eprintln!("{}  unscored  no in-universe reads", item.id);
            cases.push(CaseRecord {
                id: item.id.clone(),
                status: "unscored".to_string(),
                message: Some("no in-universe reads".to_string()),
                search_spend_usd: None,
                floors: Vec::new(),
            });
            continue;
        }
        eprintln!("{}  searching", item.id);
        match search_one(repo, &item.proposition, judge) {
            Ok(ran) => {
                search_spend += ran.spend;
                let points =
                    metrics::score_floors(&ran.full, &ran.decisions, &baseline.evidence, &FLOORS)?;
                print_case(&item.id, &points, ran.spend, &ran.trace);
                if let Some(ideal) = metrics::ideal_pruned(&ran.full, &baseline.evidence)? {
                    ideals.push(ideal);
                }
                cases.push(CaseRecord {
                    id: item.id.clone(),
                    status: "scored".to_string(),
                    message: None,
                    search_spend_usd: Some(ran.spend),
                    floors: floor_records(&points),
                });
                points_by_case.push(points);
            }
            Err(failure) => {
                search_spend += failure.spend;
                eprintln!("{}  failed  {}", item.id, failure.message);
                cases.push(CaseRecord {
                    id: item.id.clone(),
                    status: "failed".to_string(),
                    message: Some(failure.message),
                    search_spend_usd: Some(failure.spend),
                    floors: Vec::new(),
                });
            }
        }
    }
    let mut suite = suite_summary(&points_by_case, &cases);
    suite.ideal_pruned = metrics::mean(&ideals);
    let curve = curve(&points_by_case);
    print_suite(&suite, search_spend, &baselines);
    let failed = cases.iter().any(|case| case.status == "failed");
    if judge == JudgeKind::Jev {
        let (estimated, partial) = baseline_cost(&baselines);
        let (git_sha, git_dirty) = git_state(repo);
        let run = HistoryRun {
            git_sha,
            git_dirty,
            scored_at_ms: now_ms(),
            judge: judge.as_str().to_string(),
            search_spend_usd: search_spend,
            baseline_estimated_usd: estimated,
            baseline_estimate_partial: partial,
            suite,
            curve,
            cases,
        };
        let history_path = dir.join("history.jsonl");
        let mut runs = load_history(&history_path)?;
        runs.push(run);
        write_history(&history_path, &runs)?;
        fs::write(dir.join("pareto.svg"), plot::render(&runs))?;
        eprintln!("history {}", history_path.display());
    }
    if failed {
        return Err("one or more searches failed".into());
    }
    Ok(())
}

struct Ran {
    full: Filesystem,
    decisions: Vec<autoground::search::Decision>,
    spend: f64,
    trace: PathBuf,
}

struct SearchFailure {
    spend: f64,
    message: String,
}

fn search_one(repo: &Path, proposition: &str, judge: JudgeKind) -> Result<Ran, SearchFailure> {
    let universe = match Filesystem::at_root(repo) {
        Ok(universe) => universe,
        Err(err) => {
            return Err(SearchFailure {
                spend: 0.0,
                message: err.to_string(),
            })
        }
    };
    let full = universe.clone();
    let log = match Log::create() {
        Ok(log) => log,
        Err(err) => {
            return Err(SearchFailure {
                spend: 0.0,
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
            Err(err) => Err(SearchFailure {
                spend: 0.0,
                message: err.to_string(),
            }),
        },
    };
    match result {
        Ok(view) => Ok(Ran {
            full,
            decisions: view.trace,
            spend: view.usage.spend_usd,
            trace,
        }),
        Err(failure) => Err(SearchFailure {
            spend: failure.spend,
            message: format!("{}  trace {}", failure.message, trace.display()),
        }),
    }
}

fn wait<O: Oracle>(
    universe: Filesystem,
    oracle: O,
    proposition: &str,
    log: &Log,
) -> Result<autoground::search::View<Filesystem>, SearchFailure> {
    let mut session = Search::new(universe, oracle, DEFAULT_BUDGET_BYTES);
    session.set_log(log.clone());
    let ticket = session.submit(proposition);
    match session.wait(ticket) {
        Ok(view) => Ok(view),
        Err(err) => Err(SearchFailure {
            spend: err.usage.spend_usd,
            message: err.to_string(),
        }),
    }
}

fn print_case(id: &str, points: &[FloorPoint], spend: f64, trace: &Path) {
    let Some(point) = points
        .iter()
        .find(|point| (point.floor - SUPPORT).abs() < 1e-9)
    else {
        return;
    };
    let total = point.selected + point.missed.len();
    eprintln!(
        "{id}  pruned {}  evidence {} ({}/{})  {}  trace {}",
        fmt_pct(point.pruned),
        fmt_pct(point.evidence),
        point.selected,
        total,
        format_usd(spend),
        trace.display()
    );
    for path in &point.missed {
        eprintln!("  missed {path}");
    }
}

fn print_suite(
    suite: &SuiteSummary,
    search_spend: f64,
    baselines: &std::collections::BTreeMap<String, Baseline>,
) {
    let (estimated, partial) = baseline_cost(baselines);
    let baseline = match estimated {
        Some(usd) => format_usd(usd),
        None => "n/a".to_string(),
    };
    let partial = if partial { " partial" } else { "" };
    eprintln!(
        "suite @ {:.2}  pruned {}  evidence {}  micro {}  evidence-only {}  scored {}  unscored {}  failed {}  search {}  baseline {baseline}{partial}",
        suite.floor,
        fmt_pct(suite.mean_pruned),
        fmt_pct(suite.mean_evidence),
        fmt_pct(suite.micro_evidence),
        fmt_pct(suite.ideal_pruned),
        suite.scored,
        suite.unscored,
        suite.failed,
        format_usd(search_spend),
    );
}

fn suite_summary(points: &[Vec<FloorPoint>], cases: &[CaseRecord]) -> SuiteSummary {
    let mut pruned = Vec::new();
    let mut evidence = Vec::new();
    let mut selected = 0usize;
    let mut total = 0usize;
    for case_points in points {
        if let Some(point) = case_points
            .iter()
            .find(|point| (point.floor - SUPPORT).abs() < 1e-9)
        {
            if let Some(value) = point.pruned {
                pruned.push(value);
            }
            if let Some(value) = point.evidence {
                evidence.push(value);
            }
            selected += point.selected;
            total += point.selected + point.missed.len();
        }
    }
    SuiteSummary {
        floor: SUPPORT,
        mean_pruned: metrics::mean(&pruned),
        mean_evidence: metrics::mean(&evidence),
        micro_evidence: if total == 0 {
            None
        } else {
            Some(selected as f64 / total as f64)
        },
        scored: points.len(),
        unscored: cases
            .iter()
            .filter(|case| case.status == "unscored")
            .count(),
        failed: cases.iter().filter(|case| case.status == "failed").count(),
        ideal_pruned: None,
    }
}

fn redraw(repo: &Path, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let propositions = load_propositions(&dir.join("propositions.json"))?;
    let baselines = load_baselines(dir, &propositions)?;
    let universe = Filesystem::at_root(repo)?;
    let history_path = dir.join("history.jsonl");
    let mut runs = load_history(&history_path)?;
    for run in &mut runs {
        if run.suite.ideal_pruned.is_some() {
            continue;
        }
        let mut ideals = Vec::new();
        for case in &run.cases {
            if case.status != "scored" {
                continue;
            }
            let Some(baseline) = baselines.get(&case.id) else {
                continue;
            };
            if let Some(ideal) = metrics::ideal_pruned(&universe, &baseline.evidence)? {
                ideals.push(ideal);
            }
        }
        run.suite.ideal_pruned = metrics::mean(&ideals);
    }
    write_history(&history_path, &runs)?;
    fs::write(dir.join("pareto.svg"), plot::render(&runs))?;
    if let Some(ideal) = runs.iter().rev().find_map(|run| run.suite.ideal_pruned) {
        eprintln!("evidence only {}", fmt_pct(Some(ideal)));
    }
    Ok(())
}

fn curve(points: &[Vec<FloorPoint>]) -> Vec<CurvePoint> {
    FLOORS
        .iter()
        .map(|&floor| {
            let mut pruned = Vec::new();
            let mut evidence = Vec::new();
            for case_points in points {
                if let Some(point) = case_points
                    .iter()
                    .find(|point| (point.floor - floor).abs() < 1e-9)
                {
                    if let Some(value) = point.pruned {
                        pruned.push(value);
                    }
                    if let Some(value) = point.evidence {
                        evidence.push(value);
                    }
                }
            }
            CurvePoint {
                floor,
                mean_pruned: metrics::mean(&pruned),
                mean_evidence: metrics::mean(&evidence),
            }
        })
        .collect()
}

fn floor_records(points: &[FloorPoint]) -> Vec<FloorRecord> {
    points
        .iter()
        .map(|point| FloorRecord {
            floor: point.floor,
            pruned: point.pruned,
            evidence: point.evidence,
            bytes_before: point.bytes_before,
            bytes_after: point.bytes_after,
            selected: point.selected,
            missed: point.missed.clone(),
        })
        .collect()
}

fn baseline_cost(baselines: &std::collections::BTreeMap<String, Baseline>) -> (Option<f64>, bool) {
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

fn load_baselines(
    dir: &Path,
    propositions: &[Proposition],
) -> Result<std::collections::BTreeMap<String, Baseline>, Box<dyn std::error::Error>> {
    let mut missing = Vec::new();
    let mut baselines = std::collections::BTreeMap::new();
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
            "missing baseline for {}. Run search-eval baseline",
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

fn universe_leaves(universe: &Filesystem) -> Result<HashSet<String>, UniverseError> {
    let mut leaves = HashSet::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in universe.children(&dir)? {
            if universe.is_leaf(&child)? {
                leaves.insert(child);
            } else {
                stack.push(child);
            }
        }
    }
    Ok(leaves)
}

fn run_grok(repo: &Path, prompt: &str) -> Result<Value, String> {
    let prompt_path = std::env::temp_dir().join(format!("search-eval-prompt-{}.txt", now_ms()));
    fs::write(&prompt_path, prompt).map_err(|err| err.to_string())?;
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
        .arg("--yolo")
        .arg("--no-subagents")
        .arg("--disable-web-search")
        .arg("--no-auto-update")
        .arg("--max-turns")
        .arg("40")
        .arg("--tools")
        .arg("read_file,grep,list_dir")
        .arg("--deny")
        .arg("Read(evals/search/**)")
        .arg("--deny")
        .arg("Grep(evals/search/**)")
        .output();
    let _ = fs::remove_file(&prompt_path);
    let output = output.map_err(|err| format!("grok: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "grok exited {}\n{}",
            output.status,
            tail(&output.stderr)
        ));
    }
    parse_headless(&output.stdout)
}

fn parse_headless(stdout: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(stdout);
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        return Ok(value);
    }
    for line in text.lines().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            if value.get("sessionId").is_some()
                || value.get("type").and_then(Value::as_str) == Some("error")
            {
                return Ok(value);
            }
        }
    }
    Err(format!("grok stdout was not JSON\n{}", tail(stdout)))
}

fn session_id(value: &Value) -> Result<String, String> {
    let id = value
        .get("sessionId")
        .and_then(Value::as_str)
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
