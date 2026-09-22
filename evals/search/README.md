# Search evals

Each proposition is one atomic claim about this repository. Search is scored on the window it keeps, not on a true or false verdict.

Two numbers:

- **Pruned** — share of universe bytes (`Universe::estimate`) dropped from the window.
- **Evidence selected** — share of evidence files still in the window.

Evidence is the files a Grok session opened with `read_file` while looking at that proposition on its own. Those sessions are stored under `baseline/` and reused. `baseline --refresh` replaces them. Grep and directory listings are navigation, recorded on the baseline and left out of the evidence set.

`score` runs one search per proposition, the same isolation as the agent. A later comparison can batch propositions in one flush. The shipped prune floor is 0.85. The same directory decisions are replayed at 0.50, 0.70, 0.85, 0.95, and 1.00, which is the curve in `pareto.svg`. Jev runs append `history.jsonl`. `--judge heuristic` prints the table and leaves history alone.

```
cargo run --bin search-eval -- baseline
cargo run --bin search-eval -- baseline --refresh
cargo run --bin search-eval -- score --judge jev
```

Baseline cost is session tokens times `prices.toml` (xAI list rates, short-context band). `cache_creation` has no published rate; a non-zero bucket is left out of the dollar sum and the estimate is marked partial. The horizontal axis of the chart is percent of universe bytes pruned. The vertical axis is percent of evidence files selected.
