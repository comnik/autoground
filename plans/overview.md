# autoground — project plan

This plan turns the README into an implementable first system, then a sequence of slices that pressure-test the four principles.

---

## Implementation status (2026-09-17)

The logic nucleus and the grounding navigator are running. Live Jev is wired as a `Judge` behind the same questions the toys already answer locally.

| Slice | State |
|---|---|
| Language (sexp parse / print, EDB vs IDB, stratification) | **done** |
| 3VL evaluator, demand-driven `exists`/`forall` | **done** |
| In-memory EDB (facts, projected closedness, set-valued bindings, provenance) | **done** |
| DuckDB on-disk artefact | **not started** (DDL is in `Store::duckdb_schema`) |
| Tree navigator (listing `Choice`, document `Choice` `{true,false,unknown}`) | **done** |
| Local heuristic judge + JSON fixtures | **done** |
| Jev HTTP judge (`POST /v1/systemone`) | **done** (CI uses a mock transport; live needs `TYPESAFE_API_KEY`) |
| CLI `autoground run MODEL [--repo DIR]` | **done** |
| Explanations beyond bindings + require verdicts + exists witness | thin |
| dir-name / import-graph / grep proposers | **not started** |
| SpaceXAI extract fallback | **not started** |

**What works today**

```
autoground run examples/layered.brief
autoground run examples/layered.brief --repo examples/toy-layered
autoground run examples/layered.brief --repo examples/toy-cycle
autoground run examples/layered.brief --repo examples/toy-split
autoground run examples/layered.brief --repo examples/toy-file
autoground run examples/layered.brief --repo examples/toy-unknown
autoground run examples/layered.brief --repo PATH --judge jev
autoground run examples/layered.brief --repo PATH --fixtures tests/fixtures/toy-layered.json
autoground run examples/layered.brief --repo PATH --judge jev --record /tmp/trace.json
autoground ground --repo examples/toy-split '(component "Frontend")'
autoground ground --repo examples/toy-split '(component ?x)' --plain
```

`ground` walks a single atom and pages through each listing/document judgement. The current directory is drawn as a tree annotated with Jev (or heuristic) transitions. Left/right arrows flip steps; `--plain` prints them all.

Outcomes on the README model: layered / split / file → GROUNDED; cycle → VIOLATED; unknown / empty EDB → UNGROUNDED. `decoy/` file bodies are not opened.

**Code map**

- `src/syntax/` — reader, AST, pretty printer
- `src/core/` — compile (EDB/IDB, stratification)
- `src/eval/` — Kleene 3VL, tabling, generator `includes` (not `file`/`defines`)
- `src/edb/` — in-memory store
- `src/navigator/` — `TreeNavigator` + `Judge` (`heuristic`, `fixture`, `jev`, `record`)
- `examples/layered.brief` + five toy worlds
- `tests/fixtures/toy-layered.json`

**Jev protocol (implemented)**

- Env: `TYPESAFE_API_KEY` (required for `--judge jev`), `TYPESAFE_BASE_URL` (default `https://api.typesafe.ai`), `TYPESAFE_DEFAULT_MODEL` (default `jev-latest`).
- Listing: one HTTP request, one `Choice` question per child, options `{skip, descend, part, whole}`. State is the atom + listing samples, never file bodies. Chunked at 100 children.
- Document: a separate request, one `Choice` over `{true, false, unknown}`. State is path + body + current bindings. Unknown is a named option, not a band on P(yes). Low-confidence `true`/`false` still leave the atom open.
- Retries with backoff on 429 / 529 / 5xx.
- `--record` wraps any judge and writes a fixture JSON on drop.

**Locate / `defines` guards (added after a live Jev toy-split collapse)**

A Jev run bound both `"Frontend"` and `"Backend"` to `api/client.py` because (1) listings `descend`ed every directory then marked source files `part`/`whole`, and (2) `defines` was asked as “evidence for Y”, so a Backend file “defined” Backend and `depends_on(Backend, Backend)` fired.

Guards now in the navigator, independent of the judge:

- File `part` is membership, not a component root.
- File `whole` binds the file only if a document Choice says it *is* the entire component; unconfirmed whole lifts to the parent directory; confident false does not.
- `defines(f, y)` is false when `f` is already a member of `y` (membership is `includes`).
- `decoy/` / `tmp/` names are skipped even if the judge says descend.

Jev listing criteria were tightened: `descend` is for containers (`src/`), `part`/`whole` for component directories, never for ordinary source files.

- EDB is in-memory only; revision-keyed DuckDB cache is not wired.
- Default `--judge` is `heuristic` so CI and keyless runs stay deterministic. Jev is opt-in.
- Path-prefix `includes` after a directory bind is already in the navigator (the “directory whole is a gift”). Named dir-name / import-graph proposers are not a separate layer yet.
- `forall` domain is bound model constants (labels), not file paths.

---

## What we are building

`autoground` takes a conceptual model of a codebase, written as an unordered bag of logical assertions, and either:

1. **Grounds it** — finds a correspondence between abstract model entities and real code entities such that every constraint holds, and prints that correspondence; or
2. **Fails deterministically** — reports which part of the model could not be made true of the code, and why.

The model is allowed to mention predicates the tool does not yet know how to compute (`file`, `includes`, `defines`, even `component`). Those are EDB. Derivation rules (`def`) are IDB and must stay deterministic. Judgement (Jev, later an LLM) is allowed only when filling EDB.

The README example is the acceptance test for the first vertical slice: two components, a derived `depends_on`, a required direction, a forbidden reverse edge, and a generic acyclicity constraint.

---

## Problem, restated precisely

A model `M` is:

- a set of **constants** (the strings in the model: `"Frontend"`, `"Backend"`)
- a set of **EDB predicates** (used, never `def`'d)
- a set of **IDB definitions** (`def`)
- a set of **constraints** (`require`)

A **grounding** of `M` against a codebase `R` is a pair:

- a **binding table** from model constants to a *set* of code entities (paths: directories or files). `"Frontend"` may be `{src/frontend/}`, `{src/ui/}`, or `{src/app.js}`; `"Backend"` may be `{api/, server/}`.
- an **EDB fact table** over a finite universe of code entities, with a completeness bit per demand

such that every `require` evaluates to **true**.

The binding table is a *system* relation, not a construct in the modeling language. The navigator writes rows when it answers existence demands like `component("Frontend")`. Later demands (`includes`, `defines`) read them. The reporter prints them.

If some needed EDB atom cannot be decided, the constraint is **unknown**, not vacuously true. If a constraint is false on the known facts, it is **violated**. Those three outcomes are the product.

Naive closed-world Datalog is the wrong default. An empty EDB makes

```
(forall [x y]
  (implies (depends_on x y) (not (depends_on y x))))
```

true. That would report “grounded” without looking at the repo. The whole design exists to not do that.

---

## Principles, and how we will pressure-test them

The README flags these as untested. Each one gets a test that would fail if we quietly violate it.

| Principle | Meaning | Pressure test |
|---|---|---|
| **Deterministic IDB and constraints** | Same EDB ⇒ same IDB and same constraint verdict. No Jev/LLM inside `def` or `require` evaluation. | Fixture EDB in tests. Bit-identical IDB tables and verdicts across runs. |
| **LLM-augmented EDB** | Undefined atoms are filled by the Jev navigator (or an optimizer that proposes candidates for the same judgement). Once written, a fact is data. | Injecting or deleting one EDB tuple changes IDB predictably. Re-run with a frozen EDB cache never calls Jev. |
| **Top-down evaluation** | Demand-driven logic *and* goal-directed navigation. Only open paths the current demand needs. | Toy repo with a decoy tree. Navigator-call log must not read `decoy/` file bodies. |
| **Stateless, cache-as-artefact** | A run is a pure function of `(model, repo revision, navigator version, optional EDB cache)`. | Delete process state, same answer. Cache keyed by git SHA + predicate + args + navigator id. |

If a later design choice fights one of these, we change the choice, not the principle, until the principle itself is falsified.

---

## Language (v0)

Stay close to the README. No schema beyond what the program itself defines.

```
program  := form*
form     := (require formula)
          | (def name [var*] formula)
formula  := (and formula*) | (or formula*) | (not formula)
          | (implies formula formula)
          | (exists [var*] formula)
          | (forall [var*] formula)
          | (name term*)
term     := var | string
```

- Strings are constants (`"Frontend"`).
- Bare symbols in argument position are variables (`x`, `f`).
- A predicate is EDB iff it is used and never `def`'d.
- `;;` comments.
- No implicit closed world. Completeness of an EDB predicate is a property of the navigator that supplied it, recorded on the atom.

v0 does **not** need `declare` or `grounder` forms.

Parser: a small S-expression reader. AST as Rust enums. Round-trip pretty-print in tests, including the README example as a golden file.

---

## Semantics

### Three values

Every formula evaluates to `true`, `false`, or `unknown`.

Kleene:

- `not`: swap true/false, unknown stays unknown
- `and`: false wins; else unknown wins; else true
- `or`: true wins; else unknown wins; else false
- `implies φ ψ` is `or(not φ, ψ)`
- `exists`: true if any witness is true; false if the relevant domain is **closed** and every witness is false; otherwise unknown
- `forall`: false if any counterexample is true; true if the relevant domain is **closed** and every element satisfies; otherwise unknown

A run succeeds only when every `require` is `true`.

### Closed vs open EDB

Each answered demand carries a completeness bit:

- **Closed** — the navigator asserts it enumerated all tuples for this predicate at these bound arguments. A confident skip of a sibling, or a `whole` bind of a directory whose files we then list, can close.
- **Open** — more tuples might exist. A low-confidence skip of `decoy/` leaves `includes("Backend", *)` open.

`forall` / negated `exists` may only return `true` when the EDB they depend on is closed for the demanded bindings. That is the CWA footgun fix.

### IDB

`def` is a conservative three-valued consequence of its body. Same facts, same completeness bits ⇒ same IDB. Recursion is allowed if it is stratified w.r.t. negation; v0 can reject unstratified programs with a clear error.

### Demand

Evaluation is top-down with tabling:

1. Start from each `require`.
2. Unfold IDB.
3. Remaining EDB atoms, with whatever arguments are bound, are **demands**.
4. The navigator answers a demand by writing tuples, completeness, bindings, and provenance.
5. Resume until fixpoint or a budget.

Table keys: `(predicate, bound-args, polarity/completeness-need)`.

### v0 universe for `forall`

v0 ranges `forall` / `exists` over **model constants that have a binding**, plus entities the navigator has produced. The tool does not invent extra components the model never named.

---

## Two nuclei

The system has two cores, and they do not mix.

- **Logic nucleus** — the 3VL evaluator. Deterministic. Unfolds `def`/`require`, emits EDB demands, never looks at the repo.
- **Grounding nucleus** — a Jev navigator. Goal-directed walk of the repo tree that answers one EDB demand. Deterministic reasoners (dir-name, grep, imports, CodeQL) are *optimizations*: they propose candidates more cheaply; the judgement, or a cached equivalent, stays the same shape.

If the Jev-only path cannot ground the northstar model, no amount of grep will save the design. Build that path first. Speed it up later.

---

## Architecture

```
model.brief  ──► parser/AST ──► demand-driven 3VL evaluator
                                    │
                                    │ demand: atom + bound args + need (truth | closed)
                                    ▼
                             Jev navigator (default EDB handler)
                    propose                         judge
              tree listing / grep              Choice at a listing
              (optimizations later)            Choice {true,false,unknown}
                                               on a document
                                    │
                                    ▼
                             EDB store (DuckDB)
                             + binding table
                             + provenance (path, Jev ids, confidence)
                                    │
                                    ▼
                          report: grounding | unknown slice | violation
```

| Layer | Responsibility |
|---|---|
| **syntax** | S-expression parser, AST, pretty printer |
| **core** | EDB vs IDB, stratification, free/bound vars |
| **eval** | 3VL, tabling, demands. Never opens the repo. |
| **edb** | DuckDB facts, completeness, bindings, provenance, revision cache |
| **navigator** | Goal-directed Jev walk that answers one demand |
| **proposers** | Optimizations that feed the navigator candidates |
| **report** | Bindings, verdicts, unknown slice, witnesses |
| **cli** | `autoground run MODEL --repo PATH` |

```text
demand { predicate, bound_args, need } × repo × bindings × cache
    → { tuples, completeness, new_bindings, provenance }
```

---

## Grounding nucleus: goal-directed Jev navigation

Jev is strongest as a **transition function on a known option set**, evaluated in one parallel pass. The option set for locating an atom is a directory listing. The option set for confirming a file is a yes/no about that document. Those are two different states, two different question types, and they should not be mixed.

### State space

A **location state** is a directory we have decided to look inside. Its transitions are its children (files and subdirectories). One System One call per state: **one `Choice` per child**, all in the same request.

State payload (keep it small — names, not bodies):

- the demanded atom, e.g. `(component "Frontend")`
- the path of this directory
- for each child: name, file-vs-dir, and a **shallow sample** of that child’s own listing (a handful of names). Root listings are often `src/`, `lib/`, `docs/`; without the sample, the first pass is blind. This sample is part of the nucleus, not a later optimization.

`Choice` criteria per child — the valid transitions:

| Transition | Meaning |
|---|---|
| **skip** | Not this atom. Do not open it. |
| **descend** | The atom may live inside. List this directory next. Files cannot descend. |
| **part** | This path is *a piece of* the atom’s denotation. Bind it. Keep judging siblings. |
| **whole** | This path *is* the atom’s denotation. Bind it. Do not require a file explosion. |

`Choice`, not a single global “pick one path”, because Backend can be `api/` **and** `server/` in the same listing. Independent per-child questions are the parallel-inference shape. `Score` with ordered levels is a possible stand-in; the four transitions are not really ordered (`descend` is not “more bound than skip”), so `Choice` is the better primitive here.

A **document state** is a single candidate *file*. One request, one `Choice` over `{true, false, unknown}`: does this document ground `(component "Frontend")`? State is the file body (trimmed to the API budget) plus the atom. Navigation never stuffs file bodies into a listing call. Unknown is a named option Jev can pick; we do not invent it by cutting P(yes).

### Procedure, for an unbound existence atom

`(component "Frontend")` with `need: truth`, no binding yet:

1. **List** the repo root. (Later: greedy samples deeper than one level. Nucleus: names + one-level sample.)
2. **Choose transitions** — one Jev pass over the listing.
3. **Walk.** `descend` → enqueue. `skip` → record as skipped, with confidence. `part` / `whole` → candidate paths.
4. **Stop descending** when the frontier is empty, a budget is hit, or we already have a confident `whole` and `need` is only `truth`.
5. **Confirm files.** Every *file* still in the candidate set gets its own document `Choice`. Directories marked `whole` or `part` are already judgements; do not explode them into per-file questions for *this* atom. File explosion is `includes`, demanded separately.
6. **Reduce** the judgements to a 3VL atom plus a binding set plus a completeness bit. Store in DuckDB with provenance (path, request id, choice, confidence, model).
7. **Later:** keep several binding sets in flight when two `whole`s conflict. Nucleus: that case is **unknown**, not a guessed pick.

### Reduce

Not a boolean. The EDB stores decided facts; Jev’s distribution dies at this threshold, and lives on only in provenance.

```
binding = { paths with whole } ∪ { paths with part }
          ∪ { files whose document Choice is true at sufficient confidence }

if two non-nested wholes: unknown, open          # later: multiple worlds
if binding nonempty:     component(...) = true
if binding empty
   and every skip/false was confident-no
   and need was only truth:  unknown             # did not prove absence
if binding empty and we needed closed absence:   false only if the tree was fully and confidently skipped
```

Nested `whole`s (`src/` and `src/frontend/`): keep the **inner** as the denotation, or the outer if Jev marked inner as `part`. Prefer the tightest `whole`. Completeness for later `includes` is easier on a directory `whole` than on a bag of files.

### Closedness

This is the part a Jev-only walk can get wrong, and it is why we kept 3VL.

- A confident `skip` is evidence that this branch has **no** members. It contributes to closedness.
- A low-confidence `skip` leaves that branch **open**. Then `not (depends_on "Backend" "Frontend")` cannot become true.
- A directory `whole` is a gift: `includes("Frontend", ?f)` can be answered by a **deterministic listing** of that directory and marked closed. That listing is an optimization of membership, not a second judgement about what Frontend *is*.
- Existence and completeness stay different needs. Finding `ui/` makes `component("Frontend")` true; it does not close `includes("Frontend", *)` if `decoy/` was an unconfident skip.

### Not every atom walks the tree

Tree navigation is for **underdetermined location**. Once a path is in hand, judgement is a document `Choice` `{true, false, unknown}`.

| Demand | What the navigator does |
|---|---|
| `component("Frontend")` | Navigate from repo root. Bind paths. |
| `includes("Frontend", ?f)` | If Frontend is bound to directories, **list them** (optimization). If bound only to files, those files are the members. If we must find more members, navigate with the atom `includes` / “is this a Frontend file?”, not a fresh `component`. |
| `includes("Frontend", "ui/app.py")` | One document Choice on that file, or path-prefix if a directory binding exists. |
| `defines("ui/app.py", "Backend")` | No walk. One document Choice: does this file refer to Backend? State = file body + current Backend binding. Import-parsing is an optimization of that Choice. |
| `file(f)` | Pure fs. No Jev. |

The evaluator still emits the same demand tree as before. Only the EDB handler changed.

### Worked: `(component "Frontend")` in four worlds

**`src/frontend/` (lock-on).** Root listing `{src/, README.md, decoy/}`. Sample of `src/` includes `frontend/`. One pass: `src/` descend (or whole, if the sample is enough — prefer descend unless confident), `decoy/` skip, README skip. Next listing `{frontend/, backend/}`: `frontend/` **whole**, `backend/` skip. Stop. Binding `{src/frontend/}`. No file Nouls. `decoy/` never opened.

**`src/ui/` (rename).** Same walk. At `src/`: `ui/` **whole**. Name match was never required.

**`src/app.js` (the component is a file).** At `src/`: `app.js` **part** or **whole** (file, so then document Choice). State = contents of `app.js`. Confident `true` → bind `{src/app.js}`. `includes("Frontend", ?f)` then has a singleton file set, not a directory to list.

**`api/` + `server/` as Backend (split).** One listing `{ui/, api/, server/, decoy/}`. Per-child Choice, label `(component "Backend")`: `ui/` skip, `api/` **part**, `server/` **part**, `decoy/` skip. Binding `{api/, server/}`. Two `part`s succeed. Two `whole`s would be unknown. No subcomponent names invented.

### Worked: the northstar `depends_on` after that

The logic-side demand tree is unchanged:

```
component("Frontend"), component("Backend")
exists f. file(f) ∧ includes("Frontend", f) ∧ defines(f, "Backend")
not depends_on("Backend", "Frontend")    # needs includes(Backend, *) closed
forall over bound component constants
```

After the navigator has bound Frontend and Backend:

- `includes("Frontend", ?f)` lists bound roots (optimization) or Nouls files if the binding is file-grained.
- `defines(f, "Backend")` is a document Choice (or import-resolution against Backend roots, as an optimization).
- The reverse edge closes only if Backend’s skipped siblings were confident.

Report for the split world:

```
GROUNDED
  "Frontend"  ←  ui/
  "Backend"   ←  api/
                 server/
  witness: ui/app.py  defines→ Backend  via contents/import
```

### What we do not do

- Do not ask Jev whether `depends_on` holds. That is IDB.
- Do not put the full constraint set in navigation state. The atom, the constant, and the listing are enough. A one-line comment from the model is allowed; the `forall` is not.
- Do not explode a `whole` directory into file Nouls to answer `component`. That mixes `component` with `includes`.
- Do not pick a single path with one global `Choice`.
- Do not rewrite the model into `Api` / `Server`.

### Deterministic reasoners, as optimizations

Same contract. They run *before* or *instead of* a Jev pass when they apply, and they record provenance as `opt:dir-name` etc. so we can audit disagreement later.

| Optimizer | Replaces |
|---|---|
| **dir-name** | The first navigation hops when a unique name match exists. Still a binding set (usually singleton). |
| **path-prefix listing** | `includes` once a directory `whole`/`part` is bound. |
| **import graph** | The document Choice for `defines(f, y)` when an import resolves under a bound root of `y`. |
| **LLM terms → grep** | Tree navigation as a *proposer*: grep yields a candidate file set; each file still gets the same document Choice. Accuracy path, not a bypass of judgement. |
| **CodeQL / semgrep** | Other proposers, same document Choice. |

The nucleus ships with **no** optimizer required. Toys should be groundable by the navigator alone (recorded Jev fixtures in CI). dir-name on `frontend/`/`backend/` is the first optimizer we add once the navigator is real, and it must agree with the fixture on that toy.

### Budgets and jagged edges

- Max depth, max Jev calls per demand, visited-set (symlinks).
- Listings larger than Jev’s Choice cap (~255) are chunked into several passes over the same state.
- File bodies larger than the state budget are head+tail trimmed; provenance records the trim.
- Navigation questions and document questions never share a request: different state.

---

## Storage and cache

DuckDB holds EDB, bindings, completeness, and provenance. The evaluator is not SQL.

On-disk cache, optional, a build artefact:

```text
.autoground/<git-sha>/edb.duckdb
```

Cache key: `(revision, predicate, bound-args, navigator-id, navigator-version)`.

Revision-level invalidation in v0. Two CLIs on the same SHA with a warm cache do not call Jev again for a closed demand.

---

## First vertical slice

Two slices, in this order:

1. **Logic nucleus (no repo).** README rules over a handmade EDB. Empty EDB ⇒ acyclicity **unknown**. Closed empty ⇒ true. Cycle ⇒ false.
2. **Grounding nucleus (Jev navigator, recorded fixtures).** Same `layered.brief` against `toy-layered/`, `toy-cycle/`, `toy-unknown/`, `toy-split/`, and `toy-file/` (`src/app.js` as Frontend). Outcomes GROUNDED / VIOLATED / UNGROUNDED. `decoy/` file bodies never enter a document Noul.

No live API in CI. A scripted navigator replays fixtures so Phase 2–4 tests do not depend on Jev.

---

## Host language: Rust

The core is an AST, a 3VL evaluator, a demand table, and a navigator that is a state machine over listings. Rust. DuckDB via the native crate. Jev is one JSON POST (`Choice` / `Score` / `Noul`); we type it ourselves. SpaceXAI is OpenAI-compatible HTTP for the later grep-terms proposer.

Stack in use: `clap`, hand-rolled sexp reader, `serde`/`serde_json`, `ureq` for Jev. DuckDB crate not added yet; the in-memory store carries the schema. Tests: unit goldens plus `tests/ground_toys.rs` and a canned System One transport (no live network in CI).

---

## Phased delivery

### Phase 0 — skeleton — **done**

Cargo crate, module layout, clap, `examples/` worlds.

### Phase 1 — language — **done**

Parser, AST, pretty printer, golden README example, EDB vs IDB, stratification.

### Phase 2 — logic nucleus — **done**

In-memory EDB + 3VL + `def`/`require`. Empty-open acyclicity is unknown; closed-empty is true; cycle is false.

### Phase 3 — DuckDB — **partial**

In-memory relations match the planned schema (facts, completeness, bindings, provenance). `Store::duckdb_schema` is the DDL. No on-disk artefact yet.

### Phase 4 — demands — **done**

Tabling evaluator. `includes` generates candidate files; `file` and `defines` only test. Scripted navigator for evaluator unit tests.

### Phase 5 — navigator + toys — **done**

`TreeNavigator` + `Judge`. Heuristic stand-in and JSON fixtures. Five toys + decoy-body assertion.

### Phase 5b — Jev HTTP — **done**

`JevJudge` implements `Judge` over `POST /v1/systemone`. Mock `Transport` in unit tests. Live path is `--judge jev`.

### Phase 6 — explanations — **thin**

CLI prints outcome, bindings, require values, exists/forall witnesses. No unsat-core slice yet.

### Phase 7 — first optimizations — **not started**

dir-name short-circuit and import-graph `defines`. Path-prefix `includes` after a directory bind is already inside the navigator.

### Phase 8 — alternate proposer — **not started**

LLM-generated search terms (SpaceXAI) → grep → same document Noul.

### Phase 9 — more proposers — **not started**

semgrep, CodeQL. Same contract.

---

## Suggested layout

```
autoground/
  README.md
  plans/overview.md
  Cargo.toml
  src/
    main.rs
    lib.rs
    syntax/
    core/
    eval/
    edb/
    navigator/       # tree, judge, heuristic, fixture, jev, record
    proposers/       # dir-name, grep, imports — later
    report/
  examples/
    layered.brief
    toy-layered/
    toy-cycle/
    toy-unknown/
    toy-split/       # ui/ + api/ + server/
    toy-file/        # src/app.js as Frontend
  tests/
    fixtures/        # recorded System One responses
```

---

## Key decisions

1. **Three-valued demand-driven evaluation, not CWA Datalog.**
2. **Judgement fills EDB only.** The navigator never evaluates `def` or `require`.
3. **Jev navigator is the grounding nucleus.** Deterministic reasoners are proposers/short-circuits, not a parallel semantic path.
4. **Per-child `Choice` over {skip, descend, part, whole}** at a listing. Independent questions, one pass. Not one global Choice.
5. **Document `Choice` `{true, false, unknown}` for candidate files.** Directory `whole`/`part` terminates `component` without file explosion. Unknown is a named option; low-confidence true/false stay open.
6. **Reduce to {true, false, unknown} + closed/open.** Choice + API confidence stay in provenance. Multiple in-flight worlds are later.
7. **Binding table is set-valued paths** (dirs or files).
8. **Rust interpreter + in-memory EDB (DuckDB schema ready).** Jev is HTTP (`ureq`).
9. **Shallow listing samples in the nucleus**, so `src/` is not a blind first hop.
10. **Stratified negation; revision-level cache; `forall` over bound constants.**

---

## Risks

- **Closedness from skips.** Treating an unconfident skip as closed recreates vacuous CWA. Completeness must follow confidence.
- **Jev as a logic engine.** Navigation state must not contain the constraint set.
- **Listing blow-up.** Monorepo roots with thousands of children need chunking and stronger samples.
- **Overlapping membership.** Allowed unless a later model `require`s disjointness.
- **Early-access Jev.** Fixtures are mandatory; the logic nucleus must run with a scripted navigator.
- **`defines` still means “refers to”.** The document Choice is the nucleus meaning; import-graph is an optimizer of it.

---

## Out of scope for now

- Rigid architecture schema, C4, GUI
- Soufflé / clingo as the *primary* engine
- Incremental file-level cache invalidation
- Unstratified negation
- Multi-repo / non-git
- Writing models with an LLM
- Multiple in-flight grounding worlds

---

## Open questions

Do not block remaining work.

1. **Low-confidence `true`/`false`.** Leave the atom open (current), or ask a follow-up? `unknown` as a named Choice already covers “cannot tell.” `τ_c` still gates API confidence.
2. **Tightest vs outermost `whole`.** Inner is better for reporting; outer is sometimes the intended component (`src/frontend/` vs `src/frontend/src/`).
3. **CLI / extension.** `.brief`, single `run`.
4. **Second *model*** (not second world). A “service owns schema” program would really exercise `defines`.
5. **Batching demands.** One listing pass that asks Frontend *and* Backend questions against the same state. Nucleus is per-atom; batching is an obvious later win.

---

## Implementation order

Phases 0–5b are in the tree (see *Implementation status*). Next: persist EDB to DuckDB, thicken explanations, then named proposers (dir-name, imports, grep) that must agree with the toy fixtures.
