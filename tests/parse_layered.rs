use autoground::{compile, parse, print_program, verbalize_source};

#[test]
fn layered_ag_parses_and_round_trips() {
    let src = std::fs::read_to_string("examples/layered.ag").unwrap();
    let program = parse(&src).expect("parse layered.ag");
    assert_eq!(program.forms.len(), 9);
    let printed = print_program(&program);
    let again = parse(&printed).expect("reparse pretty-printed model");
    assert_eq!(program, again);
    let compiled = compile(&program).expect("compile");
    assert!(compiled.idb_preds.contains("depends_on"));
    assert_eq!(compiled.requires.len(), 4);
}

#[test]
fn layered_ag_verbalizes_component() {
    let src = std::fs::read_to_string("examples/layered.ag").unwrap();
    let lines = verbalize_source(&src).expect("verbalize layered.ag");
    assert!(
        lines
            .iter()
            .any(|l| l == "\"Frontend\" is a component in the codebase"),
        "expected component verbalization, got {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("(partially) defines")),
        "expected defines verbalization, got {lines:?}"
    );
}
