---
name: eval-oracle
description: Evaluate Jev oracle prompting and state strategies against the production baseline using `qa oracle`, with optional budget-capped adversarial probing.
argument-hint: "[strategy] [budget in cents]"
disable-model-invocation: true
---

Evaluate an oracle prompting or state-structuring strategy for `autoground` against the production `default` strategy, using the repo's `qa` CLI (`cargo run --quiet -p qa -- oracle`).

## 1. Clarify the hypothesis and budget

Identify from the user's request:
- **Candidate strategy**: a built-in preset (`cargo run --quiet -p qa -- oracle --list-strategies`), a TOML strategy overlay file, or a new strategy implemented in `qa/src/strategy.rs`.
- **Target slice** (optional): e.g. `--tag sense`, `--kind implication`, or the full default suite (`qa/suites/kernel.toml` + `qa/suites/regressions.toml`).
- **Total budget** in cents (default: `1` cent for a static A/B pass; if the user supplies a larger budget, e.g. `5` or `10` cents, use the remaining budget after the static A/B run for adversarial probing in step 4).

If the user is testing a wording, criteria, or threshold tweak, write a small temporary TOML overlay rather than editing production code:

```toml
name = "my-experiment"
base = "default" # or "roles-nested", "roles-flat", "roles-augmented", "roles-nested-premises"
threshold = 0.85
joint_question = "How do the files in `files` together ground the proposition?"

[joint_criteria]
entailed = "`files` together supply information that makes the proposition true"
contradicted = "`files` together supply information that makes the proposition false"
neutral = "`files` neither support nor refute the proposition"
```

## 2. Inspect payloads before spending budget

Always run `--dry-run` first so you verify the exact JSON sent to Jev at zero cost:

```shell
cargo run --quiet -p qa -- oracle --baseline default --strategy <candidate> --dry-run
```

Confirm that:
- `default` matches production phrasing from `src/check/strategy.rs`.
- Shared-state grouping stays intact (no accidental bloat in `unique_states`).
- The candidate's `instructions`, `criteria`, and `state_keys` look exactly as intended.

## 3. Run the static A/B evaluation

Run baseline vs. candidate with `--format json` (or `human` first, `json` when tracking exact spend):

```shell
cargo run --quiet -p qa -- oracle \
  --baseline default \
  --strategy <candidate> \
  --budget <budget_cents> \
  --format json
```

For close calls or borderline changes, re-run with `--repeats 3` on the relevant slice (`--tag ...` or `--kind ...`) to check jitter.

Evaluate results in strict priority order:
1. **Soundness errors (`contradicted`/`neutral` settled as `entailed`)** — P0 for a proof kernel. A strategy that gains accuracy by increasing soundness errors is a regression unless explicitly justified.
2. **Separation margin (`entailed_margin`) and `entailed_auc` / `brier_score`** — verify that gains are genuine discrimination improvements, not uniform confidence inflation shifting everything past `0.85`. Check the `threshold_sweep` to see if the optimal bar moved.
3. **Neutral ↔ Contradicted flips** — `neutral -> contradicted` breaks `(not ...)` and sliced witnesses.
4. **Completeness, slice breakdowns (`by_kind`, `by_tag`), and metamorphic invariants** (`negation_symmetry`, `paraphrase_agreement`, `repeat_stability`).
5. **Token / latency / batching delta** — flag state/instruction changes that noticeably increase `input_tokens` or hurt batching.

Inspect the `trace.jsonl` files reported in `baseline.trace_path` and `candidate.trace_path` for every regression, fix, and large score shift to understand *why* Jev moved.

## 4. Budget-capped adversarial hunt

If budget remains after step 3 (`remaining_usd = budget_cents / 100.0 - spent_usd`) and the user asked for adversarial probing (or gave a budget beyond the static run):

Track cumulative `spend_usd` across every `qa oracle` call using `baseline.usage.spend_usd + candidate.usage.spend_usd` from `--format json`. Never exceed the user's total budget.

Repeat while `remaining_cents >= 1` (or stop early after 2 consecutive rounds find no new failure modes):
1. Pick an untested attack angle tailored to the candidate strategy, such as:
   - **Role swaps & argument permutation** (especially for structured-role strategies: reversing asymmetric roles, repeated hole names, swapping 3-way arguments, role names that collide with common words).
   - **Closed-world leakage** (true facts outside the witness, outside the current `part`, or requiring an unstated premise in `implication`).
   - **Comment / naming traps** (misleading comments, dead code, deceptive variable names, TODOs that contradict actual behavior).
   - **Subtle boundary / operator mutations** (`<` vs `<=`, `&&` vs `||`, off-by-one, negated guards).
   - **Neutral vs. contradicted traps** (irrelevant claims paired with negative phrasing, converse fallacies, affirming the consequent).
   - **Joint-witness traps** (fact requires combining 2 files; single-file trap where file A mentions an entity and file B mentions another without connecting them).
2. Write a small, self-contained temporary TOML suite under `/tmp/autoground/qa/adv-round-<n>.toml` batching 6–15 probes per shared state (keeps each round around \$0.001–\$0.003).
3. Double-check every probe's `expected` label against the witness so your ground truth is airtight — ambiguous probes waste budget and produce false signal.
4. Run:
   ```shell
   cargo run --quiet -p qa -- oracle \
     --suite /tmp/autoground/qa/adv-round-<n>.toml \
     --baseline default \
     --strategy <candidate> \
     --budget <remaining_cents_ceil> \
     --format json
   ```
5. Update `spent_usd`, inspect any failures or regressions in the trace, and refine the next round's hypotheses.

## 5. Report and graduate regressions

Conclude with a concise report:
- **Recommendation:** `keep`, `iterate`, or `revert` (and whether any threshold or wording follow-up is warranted).
- **Static A/B summary table:** accuracy, soundness errors, completeness errors, `entailed_auc`, `entailed_margin`, `brier_score`, tokens, and total spend vs. budget.
- **Adversarial findings:** the sharpest counterexamples discovered — separating cases where `candidate` failed while `default` passed from cases where *both* failed.
- **Graduation offer:** list 1–5 clean, unambiguous counterexamples worth promoting into `qa/suites/regressions.toml` and ask the user before editing `qa/suites/regressions.toml` or `src/check/strategy.rs`.
