use response_query::{parse, project, project_to_string};

fn run(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|d| panic!("parse failed: {}", d.message));
    project_to_string(input.as_bytes(), &selection)
        .unwrap_or_else(|d| panic!("projection failed: {}", d.message))
}

fn err(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|d| panic!("parse failed: {}", d.message));
    match project(input.as_bytes(), &selection) {
        Ok(out) => panic!("expected an error, got {}", String::from_utf8_lossy(&out)),
        Err(d) => d.message,
    }
}

#[test]
fn keeps_only_selected_fields() {
    assert_eq!(run("{ id }", r#"{"id":1,"name":"x"}"#), r#"{"id":1}"#);
}

#[test]
fn drops_everything_when_nothing_matches() {
    assert_eq!(run("{ missing }", r#"{"a":1,"b":{"c":2}}"#), "{}");
}

#[test]
fn projects_nested_objects() {
    assert_eq!(
        run("{ user { id } }", r#"{"user":{"id":1,"secret":2},"x":3}"#),
        r#"{"user":{"id":1}}"#
    );
}

#[test]
fn projects_arrays_of_objects() {
    assert_eq!(
        run(
            "{ items { id } }",
            r#"{"items":[{"id":1,"a":2},{"id":3}],"total":9}"#
        ),
        r#"{"items":[{"id":1},{"id":3}]}"#
    );
}

#[test]
fn projects_two_scalars_from_a_nested_object() {
    assert_eq!(
        run(
            "{ alarm { service severity } }",
            r#"{"alarm":{"service":"api","severity":"high"}}"#
        ),
        r#"{"alarm":{"service":"api","severity":"high"}}"#
    );
}

#[test]
fn slices_with_positive_bounds() {
    assert_eq!(
        run("{ items[:2] }", r#"{"items":[1,2,3,4]}"#),
        r#"{"items":[1,2]}"#
    );
}

#[test]
fn slices_with_negative_bounds() {
    assert_eq!(
        run("{ items[-2:] }", r#"{"items":[1,2,3]}"#),
        r#"{"items":[2,3]}"#
    );
}

#[test]
fn slice_and_selection_compose() {
    assert_eq!(
        run(
            "{ items[:1] { id } }",
            r#"{"items":[{"id":1,"x":0},{"id":2,"x":0}]}"#
        ),
        r#"{"items":[{"id":1}]}"#
    );
}

#[test]
fn aliases_rename_keys() {
    assert_eq!(
        run(
            "{ n: name, owner: author { login } }",
            r#"{"name":"bob","author":{"login":"b"}}"#
        ),
        r#"{"n":"bob","owner":{"login":"b"}}"#
    );
}

#[test]
fn wildcard_keeps_original_keys() {
    assert_eq!(
        run("{ meta { * } }", r#"{"meta":{"a":1,"b":[1,2]}}"#),
        r#"{"meta":{"a":1,"b":[1,2]}}"#
    );
}

#[test]
fn explicit_fields_win_over_wildcard() {
    assert_eq!(
        run("{ meta { a * } }", r#"{"meta":{"a":1,"b":2}}"#),
        r#"{"meta":{"a":1,"b":2}}"#
    );
}

#[test]
fn no_selection_means_verbatim_subtree() {
    assert_eq!(
        run(
            "{ metadata }",
            r#"{"metadata":{"deep":{"x":[1,2]}},"skip":1}"#
        ),
        r#"{"metadata":{"deep":{"x":[1,2]}}}"#
    );
}

#[test]
fn nulls_are_preserved() {
    assert_eq!(run("{ id }", r#"{"id":null}"#), r#"{"id":null}"#);
}

#[test]
fn escapes_survive_verbatim_copy() {
    assert_eq!(
        run("{ k }", r#"{"k":"a\"b\\c\nd"}"#),
        r#"{"k":"a\"b\\c\nd"}"#
    );
}

#[test]
fn output_is_compacted() {
    let input = "{\n  \"id\": 1,\n  \"name\": \"x\"\n}\n";
    assert_eq!(run("{ id }", input), r#"{"id":1}"#);
}

#[test]
fn verbatim_subtrees_are_compacted() {
    assert_eq!(
        run("{ meta }", "{\"meta\": {\n  \"a\": 1,\n  \"b\": [1, 2]\n}}"),
        r#"{"meta":{"a":1,"b":[1,2]}}"#
    );
}

#[test]
fn root_arrays_are_projected_elementwise() {
    assert_eq!(
        run("{ id }", r#"[{"id":1},{"id":2}]"#),
        r#"[{"id":1},{"id":2}]"#
    );
}

#[test]
fn mixed_arrays_pass_scalars_through() {
    assert_eq!(run("{ id }", r#"[{"id":1},5]"#), r#"[{"id":1},5]"#);
}

#[test]
fn unicode_keys_and_values_work() {
    assert_eq!(
        run("{ näme }", r#"{"näme":"välue","x":1}"#),
        r#"{"näme":"välue"}"#
    );
}

#[test]
fn comments_are_ignored() {
    let query = r#"
        # what we care about during incidents
        {
            id,      # primary key
            name
        }
    "#;
    assert_eq!(
        run(query, r#"{"id":1,"name":"x","y":2}"#),
        r#"{"id":1,"name":"x"}"#
    );
}

#[test]
fn required_field_fails_loudly() {
    let message = err("{ id! }", r#"{"name":"x"}"#);
    assert!(message.contains("required field `id`"), "{message}");
}

#[test]
fn required_field_passes_when_present() {
    assert_eq!(run("{ id! }", r#"{"id":1}"#), r#"{"id":1}"#);
}

#[test]
fn selection_into_a_scalar_is_verbatim_not_an_error() {
    // `id` is a number, so the selection set is meaningless but harmless.
    assert_eq!(run("{ id { x } }", r#"{"id":1}"#), r#"{"id":1}"#);
}

#[test]
fn query_must_start_with_a_brace() {
    let diag = parse("id").unwrap_err();
    assert!(diag.message.contains("must start with"), "{}", diag.message);
}

#[test]
fn unclosed_selection_reports_location() {
    let diag = parse("{\n  id\n").unwrap_err();
    let (line, _col) = diag.line_col("{\n  id\n");
    assert_eq!(line, 1);
}

#[test]
fn leading_utf8_bom_is_ignored() {
    // PowerShell and Excel both emit BOMs; JSON.parse rejects them outright.
    let mut input = vec![0xEF, 0xBB, 0xBF];
    input.extend_from_slice(br#"{"id":1,"secret":2}"#);
    let selection = parse("{ id }").unwrap();
    assert_eq!(
        project_to_string(&input, &selection).unwrap(),
        r#"{"id":1}"#
    );
}

#[test]
fn deep_responses_are_fine_when_the_query_is_shallow() {
    // Unselected subtrees are skipped by scanning, so response depth only
    // costs us if the query itself drills that deep.
    let mut input = String::new();
    for _ in 0..400 {
        input.push_str(r#"{"a":"#);
    }
    input.push('1');
    for _ in 0..400 {
        input.push('}');
    }

    let out = run("{ a }", &input);
    assert!(out.starts_with(r#"{"a":{"a":"#), "unexpected prefix: {out}");
    assert!(out.ends_with("}}"), "unexpected suffix: {out}");
}

const TREE: &str = r#"{
  "id": 1, "name": "root",
  "children": [
    {"id": 2, "name": "a", "children": [
      {"id": 4, "name": "a1"},
      {"id": 5, "name": "a2", "children": [{"id": 7, "name": "a2x"}]}
    ]},
    {"id": 3, "name": "b", "children": [{"id": 6, "name": "b1"}]}
  ]
}"#;

#[test]
fn recursive_walk_collects_every_id_in_document_order() {
    assert_eq!(
        run("{ ids: children...[id] }", TREE),
        r#"{"ids":[2,4,5,7,3,6]}"#
    );
}

#[test]
fn recursive_walk_keeps_structure_with_a_selection() {
    assert_eq!(
        run("{ nodes: children...{ id name } }", TREE),
        r#"{"nodes":[{"id":2,"name":"a"},{"id":4,"name":"a1"},{"id":5,"name":"a2"},{"id":7,"name":"a2x"},{"id":3,"name":"b"},{"id":6,"name":"b1"}]}"#
    );
}

#[test]
fn a_slice_bounds_the_flat_result_not_the_traversal() {
    assert_eq!(
        run("{ ids: children...[0:3][id] }", TREE),
        r#"{"ids":[2,4,5]}"#
    );
}

#[test]
fn the_walked_edge_is_never_emitted() {
    // `*` would otherwise re-emit each node's own children, which is quadratic
    // output for a tree.
    assert_eq!(
        run(
            "{ nodes: children...{ * } }",
            r#"{"children":[{"id":2,"children":[{"id":3}]}]}"#
        ),
        r#"{"nodes":[{"id":2},{"id":3}]}"#
    );
}

#[test]
fn recursive_walk_handles_a_linked_list() {
    assert_eq!(
        run(
            "{ ids: next...[id] }",
            r#"{"next":{"id":1,"next":{"id":2,"next":{"id":3}}}}"#
        ),
        r#"{"ids":[1,2,3]}"#
    );
}

#[test]
fn recursive_tree_preserves_branches_with_only_selected_fields() {
    assert_eq!(
        run("{ children** { id } }", TREE),
        r#"{"children":[{"id":2,"children":[{"id":4},{"id":5,"children":[{"id":7}]}]},{"id":3,"children":[{"id":6}]}]}"#
    );
}

#[test]
fn recursive_tree_projects_nested_definitions_at_every_level() {
    let input = r#"{"payload":[{"events":[{"items":[{"definition":{"item_id":{"value":"root"},"other":1},"children":[{"definition":{"item_id":{"value":"child"}},"children":[{"definition":{"item_id":{"value":"grandchild"}},"extra":2}]}]}]}]}]}"#;
    assert_eq!(
        run("{ payload { events { items { definition { item_id } children** { definition { item_id } } } } } }", input),
        r#"{"payload":[{"events":[{"items":[{"definition":{"item_id":{"value":"root"}},"children":[{"definition":{"item_id":{"value":"child"}},"children":[{"definition":{"item_id":{"value":"grandchild"}}}]}]}]}]}]}"#
    );
}

#[test]
fn recursive_tree_supports_linked_objects_and_aliases_only_the_starting_edge() {
    assert_eq!(
        run(
            "{ nodes: next** { id } }",
            r#"{"next":{"id":1,"next":{"id":2,"next":null}}}"#
        ),
        r#"{"nodes":{"id":1,"next":{"id":2,"next":null}}}"#
    );
}

#[test]
fn recursive_tree_keeps_null_empty_missing_and_scalar_branches() {
    assert_eq!(
        run(
            "{ children** { id } }",
            r#"{"children":[{"id":1,"children":[]},{"id":2,"children":null},{"id":3},null,4]}"#
        ),
        r#"{"children":[{"id":1,"children":[]},{"id":2,"children":null},{"id":3},null,4]}"#
    );
}

#[test]
fn recursive_tree_wildcard_does_not_copy_unprojected_children() {
    assert_eq!(
        run(
            "{ children** { * !secret } }",
            r#"{"children":[{"id":1,"secret":2,"children":[{"id":2,"secret":3}]}]}"#
        ),
        r#"{"children":[{"id":1,"children":[{"id":2}]}]}"#
    );
}

#[test]
fn recursive_tree_reports_required_fields_missing_in_deep_nodes() {
    assert!(err(
        "{ children** { id! } }",
        r#"{"children":[{"id":1,"children":[{}]}]}"#
    )
    .contains("required field `id`"));
}

#[test]
fn recursive_tree_keeps_the_existing_depth_limit() {
    let input = format!("{}null{}", r#"{"next":"#.repeat(300), "}".repeat(300));
    assert!(err("{ next** { * } }", &input).contains("deep"));
}

#[test]
fn exclusion_with_wildcard_omits_only_the_current_level() {
    let input = r#"{"secret":1,"id":2,"nested":{"secret":3}}"#;
    assert_eq!(
        run("{ * !secret }", input),
        r#"{"id":2,"nested":{"secret":3}}"#
    );
    assert_eq!(
        run("{ !secret * }", input),
        r#"{"id":2,"nested":{"secret":3}}"#
    );
}

#[test]
fn scoped_exclusion_preserves_other_fields_and_sibling_scopes() {
    assert_eq!(
        run(
            "{ * user { * !secret } }",
            r#"{"user":{"id":1,"secret":2},"other":{"secret":3}}"#
        ),
        r#"{"user":{"id":1},"other":{"secret":3}}"#
    );
}

#[test]
fn exclusions_do_not_add_implicit_wildcards_or_require_missing_fields() {
    assert_eq!(run("{ !secret }", r#"{"id":1,"secret":2}"#), "{}");
    assert_eq!(
        run("{ * !missing !absent... }", r#"{"id":1}"#),
        r#"{"id":1}"#
    );
    assert_eq!(run("{ children** { id } }", "{}"), "{}");
}

#[test]
fn recursive_exclusion_filters_verbatim_subtrees_and_mixed_arrays() {
    assert_eq!(
        run(
            "{ * !secret... }",
            r#"{"secret":0,"nested":{"id":1,"secret":2,"rows":[{"secret":3,"id":4},[{"secret":5}],null,6]}}"#
        ),
        r#"{"nested":{"id":1,"rows":[{"id":4},[{}],null,6]}}"#
    );
}

#[test]
fn recursive_exclusion_is_limited_to_its_selected_subtree() {
    assert_eq!(
        run(
            "{ * user { * !secret... } }",
            r#"{"user":{"nested":{"secret":1,"id":2}},"other":{"secret":3}}"#
        ),
        r#"{"user":{"nested":{"id":2}},"other":{"secret":3}}"#
    );
}

#[test]
fn nested_recursive_exclusions_merge_without_affecting_siblings() {
    assert_eq!(
        run(
            "{ * !secret... user { * !token... } }",
            r#"{"user":{"secret":1,"token":2,"nested":{"secret":3,"token":4,"id":5}},"other":{"secret":6,"token":7}}"#
        ),
        r#"{"user":{"nested":{"id":5}},"other":{"token":7}}"#
    );
}

#[test]
fn recursive_tree_handles_escaped_edges_and_nested_arrays() {
    assert_eq!(
        run(
            "{ children** { id } }",
            r#"{"children":[[{"children":[{"id":2}],"id":1}],{"id":3,"chi\u006cdren":[{"id":4,"ignored":5}]}]}"#
        ),
        r#"{"children":[[{"children":[{"id":2}],"id":1}],{"id":3,"children":[{"id":4}]}]}"#
    );
}

#[test]
fn exclusions_win_over_explicit_selections_and_match_source_names_before_aliasing() {
    assert_eq!(
        run("{ * hidden: secret !secret }", r#"{"secret":1,"id":2}"#),
        r#"{"id":2}"#
    );
    assert_eq!(
        run("{ * hidden: secret !hidden }", r#"{"secret":1,"id":2}"#),
        r#"{"hidden":1,"id":2}"#
    );
    assert_eq!(
        run("{ * secret! !secret }", r#"{"secret":1,"id":2}"#),
        r#"{"id":2}"#
    );
    assert!(err("{ secret! !secret }", "{}").contains("required field"));
}

#[test]
fn exclusions_handle_literal_dots_unicode_and_escaped_json_keys() {
    assert_eq!(
        run(
            "{ * !a.b !日本語... !secret... }",
            r#"{"a.b":1,"a":{"b":2,"日本語":3},"secr\u0065t":4,"nested":{"secr\u0065t":5}}"#
        ),
        r#"{"a":{"b":2},"nested":{}}"#
    );
}

#[test]
fn recursive_exclusion_composes_with_slices_and_collects() {
    assert_eq!(
        run(
            "{ !secret... rows: items[:2][id detail] }",
            r#"{"items":[{"id":1,"detail":{"secret":2,"x":3}},{"id":4,"detail":{"secret":5}},{"id":6}]}"#
        ),
        r#"{"rows":[{"id":1,"detail":{"x":3}},{"id":4,"detail":{}}]}"#
    );
    assert_eq!(
        run("{ !id... items[id] }", r#"{"items":[{"id":1},{}]}"#),
        r#"{"items":[null,null]}"#
    );
    assert_eq!(
        run(
            "{ !secret... items[detail] }",
            r#"{"items":[{"detail":{"secret":1,"id":2}},{}]}"#
        ),
        r#"{"items":[{"id":2},null]}"#
    );
}

#[test]
fn exclusions_compose_with_both_recursive_modes() {
    let input = r#"{"children":[{"id":1,"detail":{"secret":2},"children":[{"id":3,"detail":{"secret":4}}]}]}"#;
    assert_eq!(
        run("{ !secret... children** { id detail } }", input),
        r#"{"children":[{"id":1,"detail":{},"children":[{"id":3,"detail":{}}]}]}"#
    );
    assert_eq!(
        run("{ !secret... children... { id detail } }", input),
        r#"{"children":[{"id":1,"detail":{}},{"id":3,"detail":{}}]}"#
    );
    assert_eq!(
        run("{ children** { id !children } }", input),
        r#"{"children":[{"id":1}]}"#
    );
}

#[test]
fn recursive_exclusion_reports_excessive_depth_rather_than_leaking_fields() {
    let input = format!("{}0{}", r#"{"next":"#.repeat(300), "}".repeat(300));
    assert!(err("{ * !secret... }", &input).contains("deep"));
}

#[test]
fn new_syntax_rejects_ambiguous_or_meaningless_combinations() {
    for query in [
        "{ children** }",
        "{ children**[id] }",
        "{ children**[:2] { id } }",
        "{ children...** { id } }",
        "{ children**... { id } }",
        "{ *** { id } }",
        "{ !* }",
        "{ alias: !secret }",
        "{ !alias: secret }",
        "{ !secret! }",
        "{ !secret { id } }",
        "{ !secret[id] }",
        "{ !secret** { id } }",
    ] {
        assert!(parse(query).is_err(), "should reject {query}");
    }
    assert!(parse("{ id! * !secret !token... }").is_ok());
    assert!(parse("{ items[!secret *] }").is_ok());
}

#[test]
fn recursive_walk_reports_too_deep_instead_of_truncating() {
    let mut input = String::new();
    for _ in 0..300 {
        input.push_str(r#"{"id":1,"next":"#);
    }
    input.push_str(r#"{"id":1}"#);
    for _ in 0..300 {
        input.push('}');
    }

    let message = err("{ ids: next...[id] }", &input);
    assert!(message.contains("nesting limit"), "{message}");
}

#[test]
fn collect_maps_arrays_without_touching_the_tree_semantics() {
    assert_eq!(
        run(
            "{ ids: items[id] }",
            r#"{"items":[{"id":1},{"id":2,"x":9}]}"#
        ),
        r#"{"ids":[1,2]}"#
    );
}

#[test]
fn collect_with_several_fields_emits_objects() {
    assert_eq!(
        run(
            "{ rows: items[id name] }",
            r#"{"items":[{"id":1,"name":"a","x":9}]}"#
        ),
        r#"{"rows":[{"id":1,"name":"a"}]}"#
    );
}

#[test]
fn collect_keeps_positions_when_a_value_is_missing() {
    assert_eq!(
        run("{ ids: items[id] }", r#"{"items":[{"id":1},{"x":2}]}"#),
        r#"{"ids":[1,null]}"#
    );
}

#[test]
fn slice_and_collect_compose_on_a_plain_array() {
    assert_eq!(
        run(
            "{ ids: items[0:1][id] }",
            r#"{"items":[{"id":1},{"id":2}]}"#
        ),
        r#"{"ids":[1]}"#
    );
}

#[test]
fn recursive_field_without_a_selection_is_rejected() {
    let diag = parse("{ children... }").unwrap_err();
    assert!(
        diag.message.contains("needs to say what to keep"),
        "{}",
        diag.message
    );
}

#[test]
fn wildcard_cannot_be_recursive() {
    let diag = parse("{ *... }").unwrap_err();
    assert!(diag.message.contains("field name"), "{}", diag.message);
}

#[test]
fn collect_and_selection_on_the_same_field_is_ambiguous() {
    let diag = parse("{ items[id] { name } }").unwrap_err();
    assert!(diag.message.contains("ambiguous"), "{}", diag.message);
}

#[test]
fn a_slice_after_a_collect_is_rejected() {
    let diag = parse("{ items[id][0:2] }").unwrap_err();
    assert!(
        diag.message.contains("must come before"),
        "{}",
        diag.message
    );
}
