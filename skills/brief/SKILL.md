---
name: brief
description: Create a brief from the user's instructions, fill in the details from the codebase, and loop until `autoground check` passes. Use when the user runs /brief.
argument-hint: "[instructions]"
disable-model-invocation: true
compatibility: Requires the autoground CLI on PATH.
license: MIT
---

At the speed agents are moving, any dormant misunderstandings between collaborators get 
amplified much more quickly than before. Different terminology for the same concept
leaks into the codebase at every level of granularity. As a result, everyone's mental
model of the system ends up drifting from the conceptual model that is actually 
implemented.

Help the user realign their mental model with the current state of the codebase, by
creating a `brief` (explained below) according to their instructions.

Users will instruct about the concepts and relationships they want captured in the 
brief. Unless explicitly asked, don't include information about other concepts or 
relationships that exist in the codebase.

Complete this sentence before proceeding:

> The user should understand that __.

Then capture this intent behind your briefing in the `brief` you create, using the `intent` form. Do the same for any `detail` scopes you might be adding to your brief.

Use the `autoground check` utility to validate your brief. Iterate until all claims are 
verified. An assumption is taken as true. A detail is verified when its body is. If you
notice that a claim made by the user is incorrect, report this misalignment back to them.
Remember, the goal is not to get your brief to pass, but to accurately capture the state of the codebase. If your struggling to get a brief accepted, recheck your own assumptions.


## Creating a `brief`

Aligning mental models requires using the same name for things that relate in the same way,
so consistent naming is the biggest lever for preserving understanding. `brief` is a
language for establishing shared terminology.

A brief consists of *names*, *senses*, *claims*, and *witnesses*:

```brief
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts. Components and data structures in the
;; codebase, mathematical objects, or things in the real world.
(name Frontend)
(name Backend)
(name Program)
(name AST)
(name Sweep)
(name Oracle)

;; Senses define templates for attaching meaning to names and for establishing
;; relationships between them. An application fills every hole, left to right.
(sense is_data "{Name} is a data structure")        ;; single free variable
(sense is_component "{X} is a component")           ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables
(sense calls "{caller} calls {callee}")

;; Role names on the holes disambiguate the arguments.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")
(sense related "{X} is related to {Y}")
(sense depends_on "{X} depends on {Y}")

;; A statement on its own is unsupported. A claim defines the sentence.
(claim (is_component Frontend)
  (witness "src/frontend/mod.rs"))

(claim ("Sweep asks the oracle whether a path matches the filter")
  (witness "src/sweep/mod.rs"))

;; A claim can be supported by another claim. Cite a sentence already claimed.
(sense lemma-1 "If a whole number is odd, then its square is not even.")
(sense lemma-2 "If the square of a whole number is even, then that whole number is not odd.")
(claim (lemma-1)
  (witness "src/lemma.rs"))
(claim (lemma-2)
  (lemma-1))

(claim (calls Sweep Oracle)
  (and
    ("Sweep asks the oracle whether a path matches the filter")
    (claim ("Sweep's oracle posts are the Jev HTTP call")
      (witness "src/sweep/mod.rs" "src/oracle/jev.rs"))))

;; A claim that cannot be validated from one witness takes `not`, `and`, or `or` as its support.
(claim (is_component Backend)
  (not (is_data Backend)))

(claim (returns Frontend AST)
  (and
    (is_component Frontend)
    (is_data AST)
    (claim ("Frontend parses the program and returns AST")
      (witness "src/frontend/parser.rs"))))

(claim (related Frontend Backend)
  (or
    (depends_on Frontend Backend)
    (depends_on Backend Frontend)))
```

Applying a sense to names fills the template:

```
   (returns Frontend AST)
=> ("{Name} returns {Name}" Frontend AST)
=> ("Frontend returns AST")
=> Frontend returns AST
```

`assume` takes a sentence as true, with no witness. `intent` says what understanding the brief is meant to convey.

```brief
(sense axiom "The square of an even whole number is even.")
(assume (axiom))

(intent "This brief is intended to convey the compilation pipeline.")
```

Never use `assume` when making claims about the codebase. Only use `assume` when helping the user make an internally consistent argument that is independent of the codebase. If you do end up making ungrounded assumptions, call them out to the user explicitly.

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file is too: every fenced `brief` block is taken, in order, and the rest of the document is left out.

Briefs are not accepted as witnesses for other briefs, to make sure unvalidated claims are never treated as evidence.

A `detail` hides part of an argument. Names and senses declared inside it stay there. A claim inside it about a name from outside becomes visible outside. The claim still needs its own support.

```brief
(intent "Convey the relationship between front- and backend.")

(name Frontend)
(name Backend)

(sense feeds_into "{Producer} feeds into {Consumer}")

(detail
  (intent "Explain in more detail the interaction between front- and backend.")

  (name AST)
  (sense produces "{Component} produces {Representation}")
  (sense consumes "{Component} accepts {Representation} as input")

  (claim (produces Frontend AST) (witness "src/frontend/mod.rs"))
  (claim (consumes Backend AST) (witness "src/backend/mod.rs"))

  (claim (feeds_into Frontend Backend)
    (and
      (produces Frontend AST)
      (consumes Backend AST))))
```
