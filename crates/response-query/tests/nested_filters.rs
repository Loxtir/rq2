use response_query::{infer, parse, project_to_string, validate};

fn run(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|error| panic!("{query}: {error}"));
    project_to_string(input.as_bytes(), &selection).unwrap()
}

#[test]
fn nested_predicates_filter_items_before_projecting_output_fields() {
    let input = r#"{"payload":{"events":{"items":[{"definition":{"item_id":{"value":"2762467","type":"id"}},"label":"keep","secret":1},{"definition":{"item_id":{"value":"other"}},"label":"drop"}]}}}"#;
    let query = r#"{ payload { events { items[? definition { item_id { value == "2762467" } }] { definition { item_id { value } } } } } }"#;
    assert_eq!(
        run(query, input),
        r#"{"payload":{"events":{"items":[{"definition":{"item_id":{"value":"2762467"}}}]}}}"#
    );
    assert_eq!(
        run(
            r#"{ payload { events { items[? definition { item_id { value == "2762467" } }] { title: label! } } } }"#,
            input
        ),
        r#"{"payload":{"events":{"items":[{"title":"keep"}]}}}"#
    );
    assert_eq!(
        run(
            r#"{ payload { events { items[? definition { item_id { value == "2762467" } }] } } }"#,
            input
        ),
        r#"{"payload":{"events":{"items":[{"definition":{"item_id":{"value":"2762467","type":"id"}},"label":"keep","secret":1}]}}}"#
    );
}

#[test]
fn nested_predicates_support_all_comparisons_and_preserve_missing_semantics() {
    let input = r#"{"items":[{"tag":"Mike","profile":{"name":{"value":"Mike"}}},{"tag":"X","profile":{"name":{"value":"X"}}},{"tag":"null","profile":{"name":{"value":null}}},{"tag":"missing","profile":{"name":{}}},{"profile":null},{"profile":[]},{"profile":[{"name":{"value":"Mike"}}]},{"profile":{"name":false}},{},null]}"#;
    for (condition, expected) in [
        (r#"== "Mike""#, r#"{"items":["Mike"]}"#),
        (r#"!= "Mike""#, r#"{"items":["X","null"]}"#),
        (r#"in ["Mike", "X"]"#, r#"{"items":["Mike","X"]}"#),
        (r#"not in ["Mike", "X"]"#, r#"{"items":["null"]}"#),
        ("== null", r#"{"items":["null"]}"#),
        ("!= null", r#"{"items":["Mike","X"]}"#),
        ("in []", r#"{"items":[]}"#),
        ("not in []", r#"{"items":["Mike","X","null"]}"#),
    ] {
        let query = format!("{{ items[? profile {{ name {{ value {condition} }} }}][tag] }}");
        assert_eq!(run(&query, input), expected, "{condition}");
    }
}

#[test]
fn nested_predicates_keep_exact_numbers_decoded_strings_and_literal_key_names() {
    let input = r#"{"items":[{"id":1,"d\u0065finition":{"item.id":{"value":2762467.0}}},{"id":2,"definition":{"item.id":{"value":"2762467"}}},{"id":3,"definition":{"item.id":{"value":9007199254740993}}}]}"#;
    assert_eq!(
        run(
            "{ items[? definition { item.id { value == 2762467 } }][id] }",
            input
        ),
        r#"{"items":[1]}"#
    );
    assert_eq!(
        run(
            r#"{ items[? definition { item.id { value == "2762467" } }][id] }"#,
            input
        ),
        r#"{"items":[2]}"#
    );
    assert_eq!(
        run(
            "{ items[? definition { item.id { value in [9007199254740992] } }][id] }",
            input
        ),
        r#"{"items":[]}"#
    );
    assert_eq!(
        run(
            r#"{ items[? profile { value in ["雪"] }][id] }"#,
            r#"{"items":[{"id":1,"profile":{"value":"\u96ea"}}]}"#
        ),
        r#"{"items":[1]}"#
    );
}

#[test]
fn nested_predicates_compose_with_other_filters_slices_and_exclusions() {
    let input = r#"{"items":[{"id":1,"enabled":false,"profile":{"name":"Mike"}},{"id":2,"enabled":true,"profile":{"name":"Mike"}},{"id":3,"enabled":true,"profile":{"name":"Mike"}}]}"#;
    assert_eq!(
        run(
            r#"{ items[? profile { name == "Mike" }][? enabled == true][:1][id] }"#,
            input
        ),
        r#"{"items":[2]}"#
    );
    assert_eq!(
        run(
            r#"{ !profile... selected: items[? profile { name == "Mike" }][:1] { * } }"#,
            input
        ),
        r#"{"selected":[{"id":1,"enabled":false}]}"#
    );
    let query = "{ items[? # nested path\n profile { # object\n name == \"Mike\"\n }] { id } }";
    assert_eq!(
        run(query, input),
        r#"{"items":[{"id":1},{"id":2},{"id":3}]}"#
    );
}

#[test]
fn nested_predicate_schema_validation_follows_the_path() {
    let schema =
        infer(&[br#"{"items":[{"definition":{"item_id":{"value":"2762467"}},"id":1}]}"#]).unwrap();
    let query =
        parse(r#"{ items[? definition { item_id { value in ["2762467"] } }] { id } }"#).unwrap();
    assert!(validate(&query, &schema).is_empty());
    let source = "{ items[? definition { item_id {\n vlaue == \"2762467\" } }] { id } }";
    let diagnostics = validate(&parse(source).unwrap(), &schema);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("did you mean `value`"));
    assert_eq!(diagnostics[0].line_col(source).0, 2);
    let bad_path = parse("{ items[? id { value == 1 }] }").unwrap();
    assert!(!validate(&bad_path, &schema).is_empty());
    let array_schema = infer(&[br#"{"items":[{"profile":[{"value":1}]}]}"#]).unwrap();
    let array_path = parse("{ items[? profile { value == 1 }] }").unwrap();
    assert!(validate(&array_path, &array_schema)
        .iter()
        .any(|diag| diag.message.contains("array")));
}

#[test]
fn nested_predicates_require_one_path_ending_in_one_condition() {
    for query in [
        "{ items[? profile {}] }",
        "{ items[? profile { name }] }",
        "{ items[? profile { name == 1 age == 2 }] }",
        "{ items[? alias: profile { name == 1 }] }",
        "{ items[? profile { alias: name == 1 }] }",
        "{ items[? profile! { name == 1 }] }",
        "{ items[? profile { name! == 1 }] }",
        "{ items[? * { name == 1 }] }",
        "{ items[? !profile { name == 1 }] }",
        "{ items[? profile[id] { name == 1 }] }",
        "{ items[? profile** { name == 1 }] }",
        "{ items[? profile { name == 1 { nested } }] }",
    ] {
        assert!(parse(query).is_err(), "should reject: {query}");
    }
}

#[test]
fn nested_predicate_path_depth_is_bounded() {
    let path = |depth: usize| {
        format!(
            "{{ items[? {}value == 1{}] }}",
            "next { ".repeat(depth),
            " }".repeat(depth)
        )
    };
    assert!(parse(&path(256)).is_ok());
    let error = parse(&path(257)).unwrap_err();
    assert!(error.message.contains("too deep"));
}
