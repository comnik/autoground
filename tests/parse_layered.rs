use autoground::{compile, parse, print_program, Proposition};

#[test]
fn diagram_parses_and_compiles() {
    let src = std::fs::read_to_string("examples/diagram.lect").unwrap();
    let program = parse(&src).expect("parse diagram.lect");
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
