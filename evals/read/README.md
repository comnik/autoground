# Read evals

Each proposition is one atomic claim about this repository, the same list as `evals/search/propositions.json`. Read is scored on the truth value it returns.

Two numbers:

- **Accuracy** — share of propositions whose value matches the stored judgement. Unknown against unknown counts. A failed read stays out of the rate.
- **Cost** — dollars spent to produce that answer.

The judgement is one Grok session per proposition. The agent answers true, false, or unknown, and cites the files it opened with a short excerpt copied from each. It is told to stop once those files settle the claim. Those sessions are stored under `baseline/` and reused. `baseline --refresh` replaces them. A citation has to be a file the session opened, and each line of the excerpt has to occur in that file. A wrapped doc comment and a `[`path`]` link count as the sentence they display. Provenance stays on the judgement. A miss prints the reader's evidence paths beside those citations.

`score` runs two passes. The first reads each proposition on its own. The second submits every proposition and waits once, so they share the search and the file bodies. A file entailed or contradicted at 0.85 stops that read. Both points are appended to `history.jsonl`. `pareto.svg` draws the latest score only, individual and batch; the batch point is labeled `batch`. The baseline point sits at the agents' estimated cost and accuracy 1. Older scores stay in the history file. `--judge heuristic` prints both tables and leaves history alone. `--within src` reads only that directory.

The suite includes absence claims and claims about more than one file. Each proposition is scored the same way. A proposition the reader does not witness can come back false, including an absence claim.

```
cargo run --bin read-eval -- baseline
cargo run --bin read-eval -- baseline --refresh
cargo run --bin read-eval -- score --judge jev --within src
```

Baseline cost is session tokens times `evals/search/prices.toml` (xAI list rates, short-context band). `cache_creation` has no published rate; a non-zero bucket is left out of the dollar sum and the estimate is marked partial. The horizontal axis is cost in dollars on a log scale. The vertical axis is accuracy in percent.

Wall time is recorded on both sides. Each baseline stores the Grok process time, and a score stores the pass time. The recap prints the read pass as `read <cost> in <time>` and the agent sum as `baseline <cost> in <time>`. The `each` total is one judgement after another. The `batch` total is one shared wait.
