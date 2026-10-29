use response_query::{infer, parse, project_to_string, validate};

fn run(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|error| panic!("{query}: {error}"));
    project_to_string(input.as_bytes(), &selection).unwrap()
}

#[test]
fn filters_remove_objects_but_field_conditions_only_remove_fields() {
    let input =
        r#"{"profiles":[{"name":"Mike","age":30},{"name":"Anna","age":25},{"name":"X","age":40}]}"#;
    assert_eq!(
        run(r#"{ profiles[? name == "Mike"] { name age } }"#, input),
        r#"{"profiles":[{"name":"Mike","age":30}]}"#
    );
    assert_eq!(
        run(r#"{ profiles[? name != "Mike"] { name age } }"#, input),
        r#"{"profiles":[{"name":"Anna","age":25},{"name":"X","age":40}]}"#
    );
    assert_eq!(
        run(
            r#"{ profiles[? name in ["Mike", "X"]] { name age } }"#,
            input
        ),
        r#"{"profiles":[{"name":"Mike","age":30},{"name":"X","age":40}]}"#
    );
    assert_eq!(
        run(
            r#"{ profiles[? name not in ["Mike", "X"]] { name age } }"#,
            input
        ),
        r#"{"profiles":[{"name":"Anna","age":25}]}"#
    );
    assert_eq!(
        run(r#"{ profiles { name in ["Mike", "X"] age } }"#, input),
        r#"{"profiles":[{"name":"Mike","age":30},{"age":25},{"name":"X","age":40}]}"#
    );
}

#[test]
fn membership_uses_exact_type_sensitive_scalar_equality() {
    let input = r#"[{"v":1e0},{"v":"1"},{"v":true},{"v":null},{"v":{}},{"v":[]},{}]"#;
    assert_eq!(
        run(r#"{ v in [1.0, "1", true, null] }"#, input),
        r#"[{"v":1e0},{"v":"1"},{"v":true},{"v":null},{},{},{}]"#
    );
    assert_eq!(
        run("{ v not in [1, null] }", input),
        r#"[{},{"v":"1"},{"v":true},{},{"v":{}},{"v":[]},{}]"#
    );
    assert_eq!(
        run(
            "{ v in [9007199254740992, 0.1] }",
            r#"[{"v":9007199254740993},{"v":0.10000000000000001}]"#
        ),
        "[{},{}]"
    );
    assert_eq!(
        run(
            r#"{ v in ["雪", "😀", "a:b]#"] }"#,
            r#"[{"v":"\u96ea"},{"v":"\ud83d\ude00"},{"v":"a:b]#"}]"#
        ),
        r#"[{"v":"\u96ea"},{"v":"\ud83d\ude00"},{"v":"a:b]#"}]"#
    );
}

#[test]
fn missing_fields_never_match_and_empty_lists_have_defined_results() {
    let input = r#"{"items":[{"v":null},{"v":1},{},null,1,[{"v":1}]]}"#;
    assert_eq!(run("{ items[? v in []] }", input), r#"{"items":[]}"#);
    assert_eq!(
        run("{ items[? v not in []] }", input),
        r#"{"items":[{"v":null},{"v":1}]}"#
    );
    assert_eq!(
        run("{ items[? v not in [1]] }", input),
        r#"{"items":[{"v":null}]}"#
    );
    assert_eq!(
        run("{ items[? v == null] }", input),
        r#"{"items":[{"v":null}]}"#
    );
    assert_eq!(
        run("{ items[? v != null] }", input),
        r#"{"items":[{"v":1}]}"#
    );
    assert_eq!(run("{ v in [] }", r#"[{"v":1},{}]"#), "[{},{}]");
    assert_eq!(run("{ v not in [] }", r#"[{"v":1},{}]"#), r#"[{"v":1},{}]"#);
    assert_eq!(run("{ items[? v == 1] }", "{}"), "{}");
    assert_eq!(
        run("{ items[? v == 1] }", r#"{"items":[]}"#),
        r#"{"items":[]}"#
    );
}

#[test]
fn slices_filters_and_collects_compose_in_source_order() {
    let input = r#"{"items":[{"id":1,"ok":false},{"id":2,"ok":true},{"id":3,"ok":true}]}"#;
    assert_eq!(
        run("{ items[? ok == true][:1][id] }", input),
        r#"{"items":[2]}"#
    );
    assert_eq!(
        run("{ items[:1][? ok == true][id] }", input),
        r#"{"items":[]}"#
    );
    assert_eq!(
        run("{ items[? ok == true][-1:] { id } }", input),
        r#"{"items":[{"id":3}]}"#
    );
    assert_eq!(
        run("{ items[? ok == true][? id != 2][{ id ok }] }", input),
        r#"{"items":[{"id":3,"ok":true}]}"#
    );
    assert_eq!(
        run("{ items[? ok == true][id in [2]] }", input),
        r#"{"items":[{"id":2},{}]}"#
    );
    assert_eq!(
        run(
            r#"{ items[name in ["a]:b"]] }"#,
            r#"{"items":[{"name":"a]:b"},{"name":"other"}]}"#
        ),
        r#"{"items":[{"name":"a]:b"},{}]}"#
    );
}

#[test]
fn filtering_precedes_projection_and_preserves_aliases_and_exclusions() {
    let input = r#"{"items":[{"name":"Mike","age":30,"secret":1},{"name":"Anna","secret":2}]}"#;
    assert_eq!(
        run(
            r#"{ people: items[? name == "Mike"] { years: age! } }"#,
            input
        ),
        r#"{"people":[{"years":30}]}"#
    );
    assert_eq!(
        run(r#"{ * items[? name == "Mike"] { * !secret } }"#, input),
        r#"{"items":[{"name":"Mike","age":30}]}"#
    );
    assert_eq!(
        run(r#"{ * !name... items[? name == "Mike"] }"#, input),
        r#"{"items":[{"age":30,"secret":1}]}"#
    );
    assert_eq!(
        run(
            r#"{ * alias: name in ["Mike"] }"#,
            r#"{"name":"Anna","age":25}"#
        ),
        r#"{"age":25}"#
    );
    assert_eq!(
        run(r#"{ name! not in ["Mike"] }"#, r#"{"name":"Mike"}"#),
        "{}"
    );
    assert!(project_to_string(b"{}", &parse("{ name! in [] }").unwrap()).is_err());
}

#[test]
fn nested_filters_comments_and_literal_field_names_work() {
    let query = "{ groups { people[ # array filter\n ? näme # original key\n in [\"雪\", # accepted name\n \"a:b]#\"]] { age } } }";
    let input = r#"{"groups":[{"people":[{"n\u00e4me":"\u96ea","age":1},{"näme":"a:b]#","age":2},{"näme":"other","age":3}]}]}"#;
    assert_eq!(
        run(query, input),
        r#"{"groups":[{"people":[{"age":1},{"age":2}]}]}"#
    );
    assert_eq!(
        run(
            r#"{ items[? profile.name == "Mike"] }"#,
            r#"{"items":[{"profile.name":"Mike"},{"profile":{"name":"Mike"}}]}"#
        ),
        r#"{"items":[{"profile.name":"Mike"}]}"#
    );
    assert_eq!(
        run(
            "{ in not inside nothing }",
            r#"{"in":1,"not":2,"inside":3,"nothing":4}"#
        ),
        r#"{"in":1,"not":2,"inside":3,"nothing":4}"#
    );
    assert_eq!(
        run(
            r#"{ children** { name in ["Mike"] } }"#,
            r#"{"children":[{"name":"Mike","children":[{"name":"Anna"}]}]}"#
        ),
        r#"{"children":[{"name":"Mike","children":[{}]}]}"#
    );
}

#[test]
fn filter_fields_are_schema_checked_without_becoming_selected_output() {
    let schema =
        infer(&[br#"{"items":[{"name":"Mike","age":30}],"profile":{"name":"Mike"}}"#]).unwrap();
    for query in [
        r#"{ items[? name in ["Mike", "X"]] { age } }"#,
        r#"{ items[? name != "X"][age] }"#,
        r#"{ items { name not in [] } }"#,
    ] {
        assert!(
            validate(&parse(query).unwrap(), &schema).is_empty(),
            "{query}"
        );
    }
    let source = "{ items[?\n nane == \"Mike\"] { age } }";
    let diagnostics = validate(&parse(source).unwrap(), &schema);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("did you mean `name`"));
    assert_eq!(diagnostics[0].line_col(source).0, 2);
    assert!(!validate(&parse(r#"{ profile[? name == "Mike"] }"#).unwrap(), &schema).is_empty());
}

#[test]
fn malformed_filters_and_membership_lists_are_rejected() {
    for query in [
        "{ items[?] }",
        "{ items[? name] }",
        "{ items[? name == 1 age == 2] }",
        "{ items[? alias: name == 1] }",
        "{ items[? name! == 1] }",
        "{ items[? name == 1 { age }] }",
        "{ items[? * == 1] }",
        "{ items[id][? name == 1] }",
        "{ name in [ }",
        "{ name not in 1 }",
        "{ name in [{}] }",
        "{ name in [[1]] }",
        "{ name in [NaN] }",
        "{ name in [01] }",
        "{ name in [1 2] }",
        "{ name in [1,] }",
        "{ name in [,1] }",
        "{ name in [1,,2] }",
        "{ name in [\"bad\\q\"] }",
        "{ name in [1] == 1 }",
    ] {
        assert!(parse(query).is_err(), "should reject: {query}");
    }
    for input in [r#"{"items":null}"#, r#"{"items":{}}"#, r#"{"items":1}"#] {
        let query = parse("{ items[? name == 1] }").unwrap();
        assert!(project_to_string(input.as_bytes(), &query)
            .unwrap_err()
            .message
            .contains("array"));
    }
}

#[test]
fn filters_inside_recursive_selections_keep_tree_behavior_and_depth_limits() {
    let input = r#"{"children":[{"items":[{"id":1},{"id":2}],"children":[{"items":[{"id":2}]}]}]}"#;
    assert_eq!(
        run("{ children** { items[? id in [1]] { id } } }", input),
        r#"{"children":[{"items":[{"id":1}],"children":[{"items":[]}]}]}"#
    );
    let nested = format!("{}0{}", "[".repeat(270), "]".repeat(270));
    let input = format!("{{\"items\":[{{\"id\":2,\"deep\":{nested}}}]}}");
    // Unselected deep values use the existing iterative scanner, not recursion.
    assert_eq!(run("{ items[? id == 1] }", &input), r#"{"items":[]}"#);
    let query = parse("{ items[? id == 2] { deep { id } } }").unwrap();
    assert!(project_to_string(input.as_bytes(), &query).is_err());
}
