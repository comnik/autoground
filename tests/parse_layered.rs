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

(depends_on Frontend Backend)

(not (depends_on Backend Frontend))

(or
  (calls Oracle TypeSafe)
  (\"Oracle calls the TypeSafe API\"))

(and
  (\"Frontend is a component of the system\")
  (\"{Name} is a component of the system\" Backend)
  (\"{caller} calls {callee}\" Frontend Backend))";
    let program = parse(&src).expect("parse diagram.brief");
    let printed = print_program(&program);
    let again = parse(&printed).expect("reparse");
    assert_eq!(program, again);

    let compiled = compile(&program).expect("compile");
    assert!(compiled.claims.iter().any(|claim| {
        matches!(claim, Proposition::Sentence(text) if text == "Frontend depends on Backend")
    }));
    assert!(compiled
        .claims
        .iter()
        .any(|claim| matches!(claim, Proposition::Not(_))));
}
