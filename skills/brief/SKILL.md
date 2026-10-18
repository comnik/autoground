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

Use the `autoground check` utility to validate your brief. Iterate until all claims are 
verified. If you notice that a claim made by the user is incorrect, report this 
misalignment back to them.


## Creating a `brief`

Aligning mental models requires using the same name for things that relate in the same way,
so consistent naming is the biggest lever for preserving understanding. `brief` is a
language for establishing shared terminology.

A brief consists of *names*, *senses*, and *witnesses*:

```brief
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts. Components and data structures in the
;; codebase, mathematical objects, or things in the real world.
(name Frontend)
(name Backend)
(name Program)
(name AST)

;; Senses express a claim about a name, or the relationship between names.
;; Unambiguous senses can be written as plain text sentences.
Program is a data structure
AST is a data structure
Frontend accepts Program
Frontend returns AST
The system convers Program to AST via Frontend

;; The explicit syntax for a sense requires quotes and parentheses.
("Program is a data structure")
("Frontend accepts Program")

;; Senses can have free variables, in which case the claim is not fully formulated yet.
("{Name} is a data structure")                      ;; single free variable
("{Name} returns {Name}")                           ;; two free variables

;; Senses can be named as well, to not have to fully spell them out everywhere.
(sense is_data "{Name} is a part of the system")    ;; single free variable
(sense is_component "{X} is a part of the system")  ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables

;; Roles name the free variables on a sense, to disambiguate.
(sense returns "{Component} returns {Data}")
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; A claim becomes verifiable when its support is a witness.
(claim ("Sweep asks the oracle whether a path matches the filter")
  (witness "src/sweep/mod.rs"))

;; Atomic claims can be negated, or composed into conjunctions and disjunctions.
(not (depends_on X Y))

(and
  (component X)
  (component Y)
  (depends_on X Y))

(or
  (part_of X Y)
  (part_of Y X))

;; A claim that cannot be validated from one file is supported by other claims.
;; A sentence already claimed is cited, not claimed again.
(claim (calls Sweep Oracle)
  (and
    ("Sweep asks the oracle whether a path matches the filter")
    (claim ("Sweep's oracle posts are the Jev HTTP call")
      (witness "src/sweep/mod.rs" "src/oracle/jev.rs"))))
```

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` 
file is too: every fenced `brief` block is taken, in order, and the rest of the 
document is left out.

Briefs are not accepted as witnesses for other briefs, to make sure unvalidated claims 
are never treated as evidence.
