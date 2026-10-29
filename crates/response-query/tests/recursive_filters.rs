use response_query::{infer, parse, project_to_string, validate};

fn run(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|error| panic!("{query}: {error}"));
    project_to_string(input.as_bytes(), &selection).unwrap()
}

const TREE: &str = r#"{"items":[{"id":0,"definition":{"item_id":{"value":"other"}},"children":[{"id":1,"definition":{"item_id":{"value":"wanted"}},"children":[{"id":2,"definition":{"item_id":{"value":"other"}},"children":[{"id":3,"definition":{"item_id":{"value":"wanted"}},"secret":1}]}]},{"id":4,"definition":{"item_id":{"value":"other"}}}]},{"id":5,"definition":{"item_id":{"value":"wanted"}},"children":[]},{"id":6,"definition":{"item_id":{"value":"other"}},"children":[]}]}"#;

#[test]
fn flat_recursive_filters_find_matches_below_non_matching_parents() {
    assert_eq!(
        run(
            r#"{ items...children[? definition { item_id { value == "wanted" } }] { id } }"#,
            TREE
        ),
        r#"{"items":[{"id":1},{"id":3},{"id":5}]}"#
    );
    assert_eq!(
        run(
            r#"{ matches: items...children[? definition { item_id { value in ["wanted"] } }][id] }"#,
            TREE
        ),
        r#"{"matches":[1,3,5]}"#
    );
    assert_eq!(
        run("{ items...children[? id in [1,3,5]][1:2][id] }", TREE),
        r#"{"items":[3]}"#
    );
}

#[test]
fn tree_recursive_filters_retain_only_matches_and_ancestor_containers() {
    assert_eq!(
        run(
            r#"{ matches: items**children[? definition { item_id { value == "wanted" } }] { id } }"#,
            TREE
        ),
        r#"{"matches":[{"children":[{"id":1,"children":[{"children":[{"id":3}]}]}]},{"id":5,"children":[]}]}"#
    );
    for mode in ["...", "**"] {
        assert_eq!(
            run(
                &format!("{{ items{mode}children[? id == 999] {{ id }} }}"),
                TREE
            ),
            r#"{"items":[]}"#
        );
    }
}

#[test]
fn same_key_recursion_and_explicit_child_edges_both_work() {
    let input = r#"{"children":[{"id":1,"children":[{"id":2}]}]}"#;
    assert_eq!(
        run("{ children...[? id == 2] { id } }", input),
        r#"{"children":[{"id":2}]}"#
    );
    assert_eq!(
        run("{ children**[? id == 2] { id } }", input),
        r#"{"children":[{"children":[{"id":2}]}]}"#
    );
    let input = r#"{"items":[{"id":1,"children":[{"id":2}]}]}"#;
    assert_eq!(run("{ items...children[id] }", input), r#"{"items":[1,2]}"#);
    assert_eq!(run("{ items**children { id } }", input), input);
}

#[test]
fn recursive_filters_do_not_project_rejected_nodes_or_leak_subtrees() {
    let input =
        r#"{"items":[{"children":[{"id":2,"secret":1,"children":[{"id":3,"secret":2}]}]}]}"#;
    assert_eq!(
        run("{ items...children[? id == 2] { id! * !secret } }", input),
        r#"{"items":[{"id":2}]}"#
    );
    assert_eq!(
        run("{ items**children[? id == 2] { id! * !secret } }", input),
        r#"{"items":[{"children":[{"id":2,"children":[]}]}]}"#
    );
    assert_eq!(
        run("{ items...children[? id == 2] { id !children } }", input),
        r#"{"items":[]}"#
    );
    assert_eq!(
        run("{ items**children[? id == 2] { id !children } }", input),
        r#"{"items":[]}"#
    );
    assert_eq!(
        run(
            "{ items...children[? id != null][? id != 3] { id } }",
            input
        ),
        r#"{"items":[{"id":2}]}"#
    );
}

#[test]
fn recursion_handles_escaped_edges_missing_keys_null_and_scalar_branches() {
    let input = r#"{"items":[{"id":1,"ch\u0069ldren":[{"id":2,"children":null},null,7,{},[]]},{"id":3,"children":false}]}"#;
    assert_eq!(
        run("{ items...children[? id in [2,3]] { id } }", input),
        r#"{"items":[{"id":2},{"id":3}]}"#
    );
    assert_eq!(
        run("{ items**children[? id in [2,3]] { id } }", input),
        r#"{"items":[{"children":[{"id":2,"children":null}]},{"id":3,"children":false}]}"#
    );
    assert_eq!(
        run("{ items...children[? id not in []] { id } }", "{}"),
        "{}"
    );
    assert_eq!(
        run(
            "{ items**children[? id not in []] { id } }",
            r#"{"items":[]}"#
        ),
        r#"{"items":[]}"#
    );
}

#[test]
fn recursive_filters_keep_required_and_depth_errors() {
    let query = parse("{ items**children[? id == 2] { missing! } }").unwrap();
    assert!(project_to_string(TREE.as_bytes(), &query)
        .unwrap_err()
        .message
        .contains("required"));
    let mut branch = r#"{"id":2}"#.to_string();
    for _ in 0..270 {
        branch = format!("{{\"id\":0,\"children\":[{branch}]}}");
    }
    let input = format!("{{\"items\":[{branch}]}}");
    for mode in ["...", "**"] {
        let query = parse(&format!("{{ items{mode}children[? id == 2] {{ id }} }}")).unwrap();
        assert!(
            project_to_string(input.as_bytes(), &query).is_err(),
            "{mode}"
        );
    }
}

#[test]
fn schemas_validate_child_edges_and_nested_predicates() {
    let schema = infer(&[TREE.as_bytes()]).unwrap();
    for mode in ["...", "**"] {
        let source = format!("{{ items{mode}children[? definition {{ item_id {{ value == \"wanted\" }} }}] {{ id }} }}");
        assert!(validate(&parse(&source).unwrap(), &schema).is_empty());
        let source = format!("{{ items{mode}chlidren[? id == 1] {{ id }} }}");
        assert!(validate(&parse(&source).unwrap(), &schema)
            .iter()
            .any(|diag| diag.message.contains("children")));
    }
}

#[test]
fn ambiguous_recursive_suffixes_and_bracket_orders_are_rejected() {
    for source in [
        "{ items**children[? id == 1][0:2] { id } }",
        "{ items**children[id] { id } }",
        "{ items...children[:2][? id == 1] { id } }",
        "{ items...children[? id == 1] }",
        "{ !items...children }",
        "{ items**children... { id } }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}
