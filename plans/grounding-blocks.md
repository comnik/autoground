# Grounding blocks

A sketch for batching Jev work. The current evaluator is demand-driven and sequential: each missing EDB atom is a navigator call. Independent document Nouls after one listing already run concurrently, but that is the small win. The large one is **many facts against the same document** (one Jev `state`, many questions).

## What is wrong today

`eval::run` walks `require`s in source order on one mutable `Store` and `Navigator`. A later require reuses EDB that an earlier one filled, so parallel requires are unsound. That reuse does not batch judgements.

Document relations are worse. `includes("ui/app.py", "api/client.py")` opens the file, asks one Choice, and drops the body. A sibling pair opens the same file again. Jev’s shape is the opposite: ingest the body once, fan out questions.

Listings have the same cut. `(component "Frontend")` and `(component "Backend")` are two tree walks. At a directory they could share one listing state and ask both atoms as Choices on each child.

## The DAG is not over requires

Requires stay an unordered set of assertions. The DAG is over **argument readiness**: which EDB you can ground once earlier bindings exist.

Compile already stratifies IDB (no negative cycles). This is a coarser, second DAG in front of eval.

On `examples/layered.brief`:

| Stratum | Work | Jev? |
|---|---|---|
| 1. Locate | Unary conceptual preds: `(component "Frontend")`, `(component "Backend")`. Tree walk. | Listing Choices; file confirmations |
| 2. Membership | `belongs_to` / files under those bindings. Path-prefix once a component is bound. | No |
| 3. Document relations | `includes(f1, f2)`, and any other EDB whose judgement is a file body. | One request per file, many questions |
| 4. IDB + requires | `depends_on`, the constraints. Kleene 3VL on a filled EDB. | No |

Edges: membership needs locate bindings; document atoms need file arguments; eval needs the EDB closed enough to decide.

Within stratum 1, one walk: at each listing, Choices for every atom still being located. Within stratum 3, partition ready ground atoms by file argument; one HTTP call per file.

## Ground tuples are not static

You cannot schedule `includes("ui/app.py", "api/client.py")` at compile time. `f1` and `f2` appear when earlier strata bind them. The DAG is over **predicate families and sorts**. At the start of a stratum, build the candidate ground atoms from the store (a join, not a blind cartesian).

For `includes` in the layered model, join along `depends_on`’s `belongs_to` pairs (Frontend files × Backend files, and the reverse for the integrity constraint). All-files × all-files is the wrong eager set.

## Eager vs demand

Filling a whole stratum asks atoms Kleene short-circuit would skip. That is extra questions for fewer round-trips and a closed EDB. After the EDB strata, requires are a deterministic query — closer to “EDB as a build artefact” than interleaving search with judgement.

Demand-driven eval remains the fallback for atoms a stratum did not propose.

## What not to do

- Do not parallelize `require`s against a shared store. Later requires quantify over constants earlier ones discovered; closedness is the same story.
- Do not treat IDB stratification as this DAG. IDB strata decide evaluation order of `def`s after EDB exists.
- Do not reopen a document per atom once stratum 3 is in play.

## Open

- How to classify an EDB pred into locate / membership / document from `declare` slots (and the current navigator heuristics).
- How much of the remaining requires/IDB to use when proposing the join for stratum 3.
- Whether locate of several constants is one shared walk or one walk per pred with shared listing cache.
- Trace and recording shape when one Jev exchange answers many atoms.
- Whether a filled EDB stratum is the right grain for the DuckDB revision cache.
