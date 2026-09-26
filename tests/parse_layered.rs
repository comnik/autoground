use autoground::{compile, parse, print_program, verbalize_source};

#[test]
fn diagram_parses_compiles_and_verbalizes() {
    let src = std::fs::read_to_string("examples/diagram.lect").unwrap();
    let program = parse(&src).expect("parse diagram.lect");
    let printed = print_program(&program);
    let again = parse(&printed).expect("reparse");
    assert_eq!(program, again);

    let compiled = compile(&program).expect("compile");
    assert_eq!(compiled.claims.len(), verbalize_source(&src).unwrap().len());
    let lines = verbalize_source(&src).unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line == "(\"Frontend depends on Backend\")"),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains("not")), "{lines:?}");
}
