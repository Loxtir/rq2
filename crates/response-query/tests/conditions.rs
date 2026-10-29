use response_query::{parse, project_to_string};

fn run(query: &str, input: &str) -> String {
    let selection = parse(query).unwrap_or_else(|error| panic!("{query}: {error}"));
    project_to_string(input.as_bytes(), &selection).unwrap()
}

#[test]
fn equality_and_inequality_omit_fields_instead_of_filtering_objects() {
    let input = r#"[{"id":1,"status":"ACTIVE"},{"id":2,"status":"DELETED"},{"id":3}]"#;
    assert_eq!(
        run(r#"{ id status == "ACTIVE" }"#, input),
        r#"[{"id":1,"status":"ACTIVE"},{"id":2},{"id":3}]"#
    );
    assert_eq!(
        run(r#"{ id status != "DELETED" }"#, input),
        r#"[{"id":1,"status":"ACTIVE"},{"id":2},{"id":3}]"#
    );
    assert_eq!(
        run(r#"{ status=="ACTIVE" }"#, input),
        r#"[{"status":"ACTIVE"},{},{}]"#
    );
    assert_eq!(
        run(r#"{ status!="ACTIVE" }"#, input),
        r#"[{},{"status":"DELETED"},{}]"#
    );
}

#[test]
fn comparisons_are_type_sensitive_and_null_is_not_a_missing_field() {
    let input = r#"[{"value":42},{"value":"42"},{"value":true},{"value":null},{}]"#;
    assert_eq!(
        run("{ value == 42 }", input),
        r#"[{"value":42},{},{},{},{}]"#
    );
    assert_eq!(
        run("{ value != 42 }", input),
        r#"[{},{"value":"42"},{"value":true},{"value":null},{}]"#
    );
    assert_eq!(
        run("{ value == true }", input),
        r#"[{},{},{"value":true},{},{}]"#
    );
    assert_eq!(
        run("{ value == null }", input),
        r#"[{},{},{},{"value":null},{}]"#
    );
    assert_eq!(
        run("{ value != null }", input),
        r#"[{"value":42},{"value":"42"},{"value":true},{},{}]"#
    );
    assert_eq!(
        run("{ value == false }", r#"{"value":false}"#),
        r#"{"value":false}"#
    );
}

#[test]
fn numbers_compare_exactly_without_float_rounding_and_preserve_original_notation() {
    for (literal, value) in [
        ("1", "1.0"),
        ("1e0", "1"),
        ("1000", "1e3"),
        ("1.25", "125e-2"),
        ("-1.25", "-0.125e1"),
        ("0", "-0.000e999"),
        ("1e999", "10e998"),
        ("9007199254740993", "9007199254740993.0"),
    ] {
        let input = format!("{{\"n\":{value}}}");
        assert_eq!(run(&format!("{{ n == {literal} }}"), &input), input);
        assert_eq!(run(&format!("{{ n != {literal} }}"), &input), "{}");
    }
    assert_eq!(
        run("{ n == 9007199254740992 }", r#"{"n":9007199254740993}"#),
        "{}"
    );
    assert_eq!(
        run("{ n != 9007199254740992.0 }", r#"{"n":9007199254740993}"#),
        r#"{"n":9007199254740993}"#
    );
    assert_eq!(run("{ n == 0.10000000000000001 }", r#"{"n":0.1}"#), "{}");
}

#[test]
fn strings_decode_json_escapes_but_matched_values_keep_their_original_bytes() {
    assert_eq!(run(r#"{ text == "" }"#, r#"{"text":""}"#), r#"{"text":""}"#);
    assert_eq!(
        run(r#"{ text == "😀" }"#, r#"{"text":"\ud83d\ude00"}"#),
        r#"{"text":"\ud83d\ude00"}"#
    );
    assert_eq!(
        run(r#"{ text == "a\"b\\c\nd" }"#, r#"{"text":"a\"b\\c\nd"}"#),
        r#"{"text":"a\"b\\c\nd"}"#
    );
    assert_eq!(
        run(r#"{ text == "雪" }"#, r#"{"text":"\u96ea"}"#),
        r#"{"text":"\u96ea"}"#
    );
}

#[test]
fn aliases_wildcards_and_exclusions_keep_their_existing_precedence() {
    assert_eq!(
        run(
            r#"{ * active: status == "ACTIVE" }"#,
            r#"{"status":"DELETED","id":1}"#
        ),
        r#"{"id":1}"#
    );
    assert_eq!(
        run(
            r#"{ * active: status == "ACTIVE" }"#,
            r#"{"status":"ACTIVE","id":1}"#
        ),
        r#"{"active":"ACTIVE","id":1}"#
    );
    assert_eq!(
        run(
            r#"{ * status == "ACTIVE" !status }"#,
            r#"{"status":"ACTIVE","id":1}"#
        ),
        r#"{"id":1}"#
    );
    assert_eq!(
        run(
            r#"{ content-type == "application/json" näme != "other" }"#,
            r#"{"content-type":"application/json","n\u00e4me":"雪"}"#
        ),
        r#"{"content-type":"application/json","näme":"雪"}"#
    );
}

#[test]
fn required_checks_presence_not_whether_the_condition_matches() {
    assert_eq!(
        run(r#"{ status! == "ACTIVE" }"#, r#"{"status":"DELETED"}"#),
        "{}"
    );
    let query = parse(r#"{ status! != "DELETED" }"#).unwrap();
    let error = project_to_string(b"{}", &query).unwrap_err();
    assert!(error.message.contains("required field `status`"));
}

#[test]
fn nullable_containers_are_compared_before_their_selection_is_projected() {
    assert_eq!(
        run(
            "{ profile != null { name } }",
            r#"{"profile":{"name":"Ada","secret":1}}"#
        ),
        r#"{"profile":{"name":"Ada"}}"#
    );
    assert_eq!(
        run("{ profile != null { name } }", r#"{"profile":null}"#),
        "{}"
    );
    assert_eq!(
        run("{ profile == null }", r#"{"profile":{"name":"Ada"}}"#),
        "{}"
    );
    assert_eq!(
        run(
            "{ items != null { id == 1 } }",
            r#"{"items":[{"id":1},{"id":2}]}"#
        ),
        r#"{"items":[{"id":1},{}]}"#
    );
}

#[test]
fn predicates_work_inside_slices_collects_and_recursive_selections() {
    let input = r#"{"items":[{"id":1},{"id":2},{}]}"#;
    assert_eq!(
        run("{ items[:2] { id == 1 } }", input),
        r#"{"items":[{"id":1},{}]}"#
    );
    assert_eq!(
        run("{ items[id == 1] }", input),
        r#"{"items":[{"id":1},{},{}]}"#
    );
    assert_eq!(
        run("{ items[{ id != 1 }] }", input),
        r#"{"items":[{},{"id":2},{}]}"#
    );
    let tree = r#"{"children":[{"id":1,"children":[{"id":2}]}]}"#;
    assert_eq!(
        run("{ children** { id == 1 } }", tree),
        r#"{"children":[{"id":1,"children":[{}]}]}"#
    );
    assert_eq!(
        run("{ children... { id != 1 } }", tree),
        r#"{"children":[{},{"id":2}]}"#
    );
}

#[test]
fn comments_and_literal_delimiters_do_not_confuse_the_condition_parser() {
    let query =
        "{\n status # check before emitting\n != # comparison literal\n \"a } ], # b\"\n id\n}";
    assert_eq!(
        run(query, r#"{"status":"other","id":7}"#),
        r#"{"status":"other","id":7}"#
    );
    assert_eq!(
        run(query, r#"{"status":"a } ], # b","id":7}"#),
        r#"{"id":7}"#
    );
}

#[test]
fn malformed_or_unsupported_conditions_report_query_locations() {
    for query in [
        "{ status = 1 }",
        "{ status === 1 }",
        "{ status !== 1 }",
        "{ status == }",
        "{ status != ACTIVE }",
        "{ status == 'ACTIVE' }",
        "{ status == [] }",
        "{ status != {} }",
        "{ status == 01 }",
        "{ status == 1. }",
        "{ status == 1e }",
        "{ status == NaN }",
        "{ status == \"bad\\q\" }",
        "{ status == 1 != 2 }",
        "{ * == 1 }",
        "{ !status == 1 }",
        "{ items[:2] == 1 }",
        "{ children** == 1 { id } }",
        "{ children... == 1 { id } }",
    ] {
        assert!(parse(query).is_err(), "should reject: {query}");
    }
    let query = "{\n  status == unquoted\n}";
    let error = parse(query).unwrap_err();
    assert_eq!(error.line_col(query).0, 2);
    assert!(error.message.contains("literal"));
}
