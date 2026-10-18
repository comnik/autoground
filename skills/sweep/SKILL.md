---
name: sweep
description: Distill the user's instructions into a brief and delegate to `autoground sweep`. Use when the user runs /sweep.
argument-hint: "[instructions]"
disable-model-invocation: true
compatibility: Requires the autoground CLI on PATH.
license: MIT
---

The user asks for a sweep when they want to (re-)establish trust in certain invariants about the codebase. Common use cases include looking for potential security issues, finding all places in the codebase where network requests or disk access occur, checking whether the current diff touches a specific behaviour, and many more.

Help the user by creating a `brief` (explained below) that captures the claims they want to sweep for.

Then use `autoground sweep` to search the codebase for files that  match those claims. Depending on the users instructions, `autoground sweep diff` might be more appropriate, to search only the files affected by the current diff. In some cases, the user might ask about an invariant in the current session. In that case, use `autoground sweep transcript` and point it at your current session transcript.

## Creating a `brief`

A sweep brief is *names*, *senses*, and sentences combined with `and`, `or`, and `not`. It has no `claim`, `witness`, `assume`, or `detail`.

```brief
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts.
(name Frontend)
(name Backend)
(name AST)
(name X)
(name Y)

;; Senses define templates. An application fills every hole, left to right.
(sense is_data "{Name} is a data structure")
(sense is_component "{X} is a component")
(sense returns "{Name} returns {Name}")
(sense depends_on "{X} depends on {Y}")
(sense part_of "{X} is part of {Y}")

;; Statements, and boolean structure over them, are the constraints.
(is_component Frontend)
(is_data AST)
(returns Frontend AST)

(not (depends_on X Y))

(and
  (is_component X)
  (is_component Y)
  (depends_on X Y))

(or
  (part_of X Y)
  (part_of Y X))
```

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file is too: every fenced `brief` block is taken, in order, and the rest of the document is left out.
