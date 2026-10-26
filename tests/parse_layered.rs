use autoground::{compile, parse, print_program, Proposition};

#[test]
fn diagram_parses_and_compiles() {
    let src = "\
(name Frontend)
(name Backend)
(name Oracle)
(name TypeSafe)

(sense depends_on \"{X} depends on {Y}\")
(sense calls \"{caller} calls {callee}\")

(claim (depends_on Frontend Backend)
  (not (depends_on Backend Frontend)))

(claim \"routes\"
  (or
    (calls Oracle TypeSafe)
    \"Oracle calls the TypeSafe API\"))

(claim \"pieces\"
  (and
    \"Frontend is a component of the system\"
    (\"{Name} is a component of the system\" Backend)
    (\"{caller} calls {callee}\" Frontend Backend)))";
    let program = parse(&src).expect("parse diagram.brief");
    let printed = print_program(&program);
    let again = parse(&printed).expect("reparse");
    assert_eq!(program, again);

    let compiled = compile(&program).expect("compile");
    assert!(compiled.claims.iter().any(|claim| {
        matches!(claim, Proposition::Claim { sentence, .. } if sentence.text == "Frontend depends on Backend")
    }));
    assert!(compiled.claims.iter().any(|claim| {
        matches!(
            claim,
            Proposition::Claim {
                support: autoground::syntax::Support::Sentence(inner),
                ..
            } if matches!(inner.as_ref(), Proposition::Not(_))
        )
    }));
}
