## The `brief` language

Informal proof is a dynamic process where proof author and verifier iteratively align their
respective mental models and expose implicit assumptions. Aligning mental models requires
using the same name for things that relate in the same way. brief is a language with two
goals: 1) help establish shared terminology, and 2) support gradual decomposition of a
complicated argument into increasingly atomic claims.

> **[brief](https://www.merriam-webster.com/dictionary/brief)**<br/>
> *"a formal written presentation of an argument that sets forth the main points with
> supporting precedents and evidence"*

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file
is too: every fenced `brief` block is taken, in order, and the rest of the document is left
out.

## Grammar

A brief is a sequence of forms. `ns`, when present, is the first form and the only one. The
ordering of all other top-level forms is irrelevant.

```
program   ::= [ns] form*
ns        ::= "(" "ns" namespace [require] ")"
namespace ::= segments separated by ".", no empty segment, no "/"
require   ::= "(" ":require" libspec+ ")"
libspec   ::= "[" namespace [":as" alias] [":refer" refer] "]"
refer     ::= ":all" | "[" ident-name* "]"
form      ::= name | sense | assume | detail | claim | and | or | not
name      ::= "(" "name" ident-name ")"
sense     ::= "(" "sense" ident-name string ")"
assume    ::= "(" "assume" sentence ")"
detail    ::= "(" "detail" form+ ")"
formula   ::= claim | and | or | not | sentence
claim     ::= "(" "claim" sentence support ")"
support   ::= witness | formula
witness   ::= "(" "witness" path+ ")"
and       ::= "(" "and" formula* ")"
or        ::= "(" "or" formula* ")"
not       ::= "(" "not" formula ")"
sentence  ::= string | "(" ident arg* ")" | "(" string arg+ ")"
arg       ::= ident | string | "(" ident ")"
ident     ::= [qualifier "/"] ident-name
path      ::= string | "$"
```

`ident-name` is an unqualified identifier. A comma is whitespace. `;;` comments through the
end of the line.

A scope is the file, or one `detail`. A `detail` contains a claim or an assumption, walking
into nested details and into `and`, `or`, and `not`. `ns` stays at the top of the file. The
file is accepted when every top-level form is true. An `or` form is true when any operand is
true. A `not` form is true when its operand is false.

`witness` is only the support of a `claim`. A path is relative to the brief. The symbol `$`
is the file a check was given. One sentence is defined once in the file, by a `claim` or an
`assume`. A claim whose support reaches its own sentence is rejected. Names and senses
declared inside a `detail` stay there. Names and senses declared outside gain the sentences
validated inside.

## Constructs

### Namespaces

An optional namespace at the beginning of a brief disambiguates its contents from others.

```brief
(ns com.example)
```

### Names

Names identify concrete or abstract concepts. Components and data structures in the
codebase, mathematical objects, or things in the real world. They are the nodes in a mental
model.

```brief
(name Frontend)
(name Backend)
(name AST)
(name User)
(name Server)
```

### Senses

Senses define templates for attaching meaning to names and for establishing relationships
between them. Senses can be of any arity, including nullary.

```brief
(sense invariant "All traffic is encrypted")        ;; no free variables

(sense is_data "{Name} is a data structure")        ;; single free variable
(sense is_component "{X} is a component")           ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables

;; Note how the variables can be named as well, to disambiguate roles in a relationship.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; Ternary relationship with three different named roles.
(sense has_permission "{subject} is granted {permission} on {object}")
```

Applying names to a sense constructs a fully formed sentence:

```
   (returns Frontend AST)
=> ("{Name} returns {Name}" Frontend AST)
=> "Frontend returns AST"
```

Nullary senses are special, because they can be used to define terminology as well:

```brief
(sense FrontTarget "the AST produced by the compiler frontend")

(claim (is_data (FrontTarget))
  (is_data AST))
```

Note how `(FrontTarget)` is applied to a sense, effectively expanding the definition in
place. The judge always sees the expanded form. Definitions are just abbreviations, they
don't make any additional inferences.

However, a nullary sense is _either_ used as a definition (like `(is_data (FrontTarget))`)
_or_ as a sentence (like `(claim (invariant))`). Using the same sense in both ways is an
error. A sense declared outside a detail is checked across the file. A sense declared inside
a detail is checked inside that detail. It is still possible to validate the definition on
its own:

```brief
(sense identifies "the code identifies {X}")

(claim (identifies (FrontTarget))
  (witness ...))
```

### Claims and Witnesses

Claims make testable propositions, by combining a fully formed sentence with a *witness*
that grounds the statement in the reality of the codebase.

```brief
(claim (is_component Frontend)
  (witness "src/frontend/mod.rs"))

(claim "The server accepts compressed payloads"
  (witness "src/server/request.rs"))

(claim "The buy button is rendered"
  (witness "/tmp/eval/screenshots/main.png"))
```

Witness paths are relative to the brief location. Typically source files should be used as
witnesses. It is possible to use PNG or JPEG images as witnesses, for example when tying a
claim to a screenshot of the live application. Briefs are not accepted as witnesses for
other briefs, to make sure unvalidated claims are never treated as evidence.

Claims can be supported by other claims as well, connected via `and`, `or`, or `not`.

```brief
(claim (returns Frontend AST)
  (and
    (is_component Frontend)
    (is_data AST)
    (claim "Frontend parses the program and returns AST"
      (witness "src/frontend/parser.rs"))))

(claim (is_component Backend)
  (not (is_data Backend)))

(claim (related Frontend Backend)
  (or
    (depends_on Frontend Backend)
    (depends_on Backend Frontend)))
```

By default, a brief is accepted by `autoground check` if all of its claims validate. `and`,
`or`, and `not` may also appear at the top-level, which allows expressing a brief that is
accepted if _any_ or _none_ of its claims validate.

```brief
(not (claim (depends_on Backend Frontend) (witness "src/backend/mod.rs")))

(or
  (claim "An incoming request enters the queue"
   (witness "src/server/mod.rs"))
  (claim "An incoming request is rejected if the queue is at capacity"
   (witness "src/server/queue.rs")))
```

### Assumptions

`assume` takes a sentence as true, with no witness.

```brief
(sense axiom "The square of an even whole number is even.")
(assume (axiom))
```

### Details

A `detail` hides part of an argument. Names and senses declared inside it stay there. A
claim inside it about a name from outside becomes visible outside. The claim still needs its
own support.

```brief
(name Frontend)
(name Backend)

(sense feeds_into "{Producer} feeds into {Consumer}")

(detail
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
