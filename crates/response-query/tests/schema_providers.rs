use response_query::openapi::Spec;
use response_query::{
    fields_at, infer, parse, scaffold, to_json_schema, validate, ScalarKind, SchemaKind,
};
use serde_json::json;

fn sample(text: &str) -> serde_json::Value {
    to_json_schema(&infer(&[text.as_bytes()]).expect("inference should succeed"))
}

#[test]
fn infers_scalars_and_objects() {
    let schema = sample(r#"{"id":1,"name":"x","ok":true,"ratio":1.5}"#);
    assert_eq!(
        schema,
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "name": { "type": "string" },
                "ok": { "type": "boolean" },
                "ratio": { "type": "number" }
            },
            "required": ["id", "name", "ok", "ratio"]
        })
    );
}

#[test]
fn infers_array_element_types() {
    let schema = sample(r#"{"items":[{"id":1},{"id":2}]}"#);
    assert_eq!(
        schema,
        json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "id": { "type": "integer" } },
                        "required": ["id"]
                    }
                }
            },
            "required": ["items"]
        })
    );
}

#[test]
fn empty_array_element_type_is_untyped() {
    let schema = sample(r#"{"items":[]}"#);
    let element = &schema["properties"]["items"]["items"];
    assert!(
        element.get("description").is_some(),
        "empty arrays cannot be typed: {element}"
    );
}

#[test]
fn all_null_field_is_nullable() {
    let schema = sample(r#"{"gone":null}"#);
    let text = serde_json::to_string(&schema).unwrap();
    assert!(text.contains("\"nullable\":true"), "{text}");
}

#[test]
fn merges_samples_into_optional_fields() {
    let a = r#"{"id":1,"name":"x"}"#;
    let b = r#"{"id":2,"extra":"y"}"#;
    let schema = to_json_schema(&infer(&[a.as_bytes(), b.as_bytes()]).unwrap());

    let required = schema.get("required").unwrap().as_array().unwrap();
    assert_eq!(
        required,
        &vec![json!("id")],
        "only id appears in every sample"
    );

    let props = schema.get("properties").unwrap().as_object().unwrap();
    assert!(props.contains_key("name"));
    assert!(props.contains_key("extra"));
}

#[test]
fn fields_at_walks_through_arrays() {
    let schema = infer(&[r#"{"items":[{"id":1,"meta":{"k":"v"}}]}"#.as_bytes()]).unwrap();
    let node = fields_at(&schema, &["items"]).expect("items");
    assert!(matches!(&node.kind, SchemaKind::Array(_)));

    let meta = fields_at(&schema, &["items", "meta"]).unwrap();
    match &meta.kind {
        SchemaKind::Object(fields) => assert!(fields.contains_key("k")),
        other => panic!("expected object, got {other:?}"),
    }

    assert!(fields_at(&schema, &["items", "nope"]).is_none());
}

#[test]
fn infers_understands_scalar_kinds() {
    let schema = infer(&[r#"{"n":1}"#.as_bytes()]).unwrap();
    match &schema.kind {
        SchemaKind::Object(fields) => match &fields.get("n").unwrap().schema.kind {
            SchemaKind::Scalar(ScalarKind::Integer) => {}
            other => panic!("expected integer, got {other:?}"),
        },
        other => panic!("expected object, got {other:?}"),
    }
}

#[test]
fn inference_rejects_absurd_nesting() {
    let mut input = String::new();
    for _ in 0..400 {
        input.push_str(r#"{"a":"#);
    }
    input.push('1');
    for _ in 0..400 {
        input.push('}');
    }
    let message = match infer(&[input.as_bytes()]) {
        Ok(_) => panic!("expected depth rejection"),
        Err(d) => d.message,
    };
    assert!(message.contains("too deep"), "{message}");
}

const RECURSIVE_SPEC: &str = r##"{
  "openapi": "3.0.0",
  "paths": {
    "/nodes/{id}": {
      "get": {
        "operationId": "getNode",
        "responses": {
          "200": {
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/Node" }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "Node": {
        "type": "object",
        "properties": {
          "id": { "type": "integer" },
          "label": { "type": "string" },
          "parent": { "$ref": "#/components/schemas/Node" },
          "children": {
            "type": "array",
            "items": { "$ref": "#/components/schemas/Node" }
          }
        }
      }
    }
  }
}"##;

const YAML_SPEC: &str = r#"
openapi: 3.0.0
paths:
  /ping:
    get:
      operationId: getPing
      responses:
        '200':
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Ping'
components:
  schemas:
    Ping:
      type: object
      required: [ok]
      properties:
        ok:
          type: boolean
        uptimeSeconds:
          type: number
        note:
          type: string
          nullable: true
"#;

#[test]
fn parses_yaml_specs() {
    let spec = Spec::parse(YAML_SPEC.as_bytes()).expect("yaml spec should parse");
    let schema = spec.response_schema("getPing").unwrap().expect("schema");

    let ok = fields_at(&schema, &["ok"]).expect("ok");
    assert!(matches!(&ok.kind, SchemaKind::Scalar(ScalarKind::Boolean)));

    match &schema.kind {
        SchemaKind::Object(fields) => {
            assert!(fields.get("ok").unwrap().required);
            assert!(!fields.get("note").unwrap().required);
            assert!(fields.get("note").unwrap().schema.nullable);
        }
        other => panic!("expected object, got {other:?}"),
    }
}

#[test]
fn rejects_documents_that_are_not_openapi() {
    let error = Spec::parse(br#"{"hello":"world"}"#).unwrap_err();
    assert!(error.message.contains("openapi"), "{}", error.message);
}

#[test]
fn scaffold_round_trips_through_parser_and_validator() {
    // The strongest check that the schema model maps onto the language: what
    // the generator emits must parse, and must validate against its own source.
    for (text, operation) in [(SPEC, "getUser"), (RECURSIVE_SPEC, "getNode")] {
        let spec = Spec::parse(text.as_bytes()).expect("spec");
        let schema = spec
            .response_schema(operation)
            .expect("no error")
            .expect("schema");

        let generated = scaffold(&schema, 3);
        let parsed = parse(&generated)
            .unwrap_or_else(|d| panic!("scaffold output does not parse: {d}\n{generated}"));

        let diags = validate(&parsed, &schema);
        assert!(
            diags.is_empty(),
            "scaffold output does not validate: {diags:?}\n{generated}"
        );
    }
}

#[test]
fn scaffold_nests_objects_within_the_depth_limit() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let generated = scaffold(&schema, 3);
    assert!(generated.contains("profile {"), "{generated}");
    assert!(generated.contains("displayName"), "{generated}");

    let shallow = scaffold(&schema, 1);
    assert!(!shallow.contains("displayName"), "{shallow}");
}

#[test]
fn a_dotted_path_gets_nesting_advice() {
    // `profile.displayName` parses as one literal key, because dots are legal
    // in field names. That is almost never what someone means.
    let spec = Spec::parse(SPEC.as_bytes()).expect("spec");
    let schema = spec
        .response_schema("getUser")
        .expect("no error")
        .expect("schema");

    let parsed = parse("{ profile.displayName }").unwrap();
    let diags = validate(&parsed, &schema);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("profile { displayName }"),
        "{}",
        diags[0].message
    );
}

#[test]
fn recursive_and_collect_queries_validate_against_a_recursive_schema() {
    let spec = Spec::parse(RECURSIVE_SPEC.as_bytes()).expect("spec");
    let schema = spec
        .response_schema("getNode")
        .expect("no error")
        .expect("schema");

    for query in [
        "{ ids: children...[id] }",
        "{ nodes: children...{ id label } }",
        "{ children** { id label } }",
        "{ * !missing !secret... children** { id } }",
    ] {
        let parsed = parse(query).unwrap();
        assert!(
            validate(&parsed, &schema).is_empty(),
            "{query}: {:?}",
            validate(&parsed, &schema)
        );
    }

    // A scalar cannot be walked, and only a list can be collected.
    let walked = parse("{ x: label...[id] }").unwrap();
    let diags = validate(&walked, &schema);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("cannot walk"),
        "{}",
        diags[0].message
    );

    let collected = parse("{ x: label[id] }").unwrap();
    let diags = validate(&collected, &schema);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("not a list"),
        "{}",
        diags[0].message
    );

    // A typo inside a collect still gets a suggestion.
    let typo = parse("{ ids: children...[idd] }").unwrap();
    let diags = validate(&typo, &schema);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("did you mean"),
        "{}",
        diags[0].message
    );
}

#[test]
fn tree_recursion_rejects_scalar_edges_but_exclusions_need_not_exist_in_the_schema() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let parsed = parse("{ id** { * } }").unwrap();
    let diags = validate(&parsed, &schema);
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("`**` cannot walk"));

    let parsed = parse("{ * !absent !nestedSecret... }").unwrap();
    assert!(validate(&parsed, &schema).is_empty());
}

#[test]
fn recursive_models_terminate_instead_of_exploding() {
    // `parent` and `children` both point back at Node. Expanding both eagerly
    // would be exponential; the ref stack must cut the cycle at the first
    // revisit. This test would hang (or OOM) before that fix.
    let spec = Spec::parse(RECURSIVE_SPEC.as_bytes()).expect("spec");
    let schema = spec
        .response_schema("getNode")
        .expect("no error")
        .expect("schema");

    // One level is typed.
    let label = fields_at(&schema, &["label"]).expect("label");
    assert!(matches!(
        &label.kind,
        SchemaKind::Scalar(ScalarKind::String)
    ));

    let children = fields_at(&schema, &["children"]).expect("children");
    match &children.kind {
        // The element type is kept, so `children...` can be validated and
        // completed. Only the expansion below it is cut.
        SchemaKind::Array(items) => match &items.kind {
            SchemaKind::Object(fields) => {
                assert!(fields.contains_key("id"));
                assert!(matches!(
                    &fields["id"].schema.kind,
                    SchemaKind::Scalar(ScalarKind::Integer)
                ));
            }
            other => panic!("expected a shallow object, got {other:?}"),
        },
        other => panic!("expected array, got {other:?}"),
    }

    // A second level is cut rather than unrolled, so the schema stays finite.
    let parent = fields_at(&schema, &["parent"]).expect("parent");
    match &parent.kind {
        SchemaKind::Object(fields) => {
            assert!(fields.contains_key("label"));
            assert!(matches!(&fields["parent"].schema.kind, SchemaKind::Any));
        }
        other => panic!("expected a shallow object, got {other:?}"),
    }

    let second = fields_at(&schema, &["children", "children"]).expect("second level");
    match &second.kind {
        SchemaKind::Array(items) => assert!(matches!(&items.kind, SchemaKind::Any)),
        other => panic!("expected array, got {other:?}"),
    }
}

#[test]
fn a_recursive_query_is_still_accepted() {
    // Degrading to `Any` must not make validation reject a nested query.
    let spec = Spec::parse(RECURSIVE_SPEC.as_bytes()).expect("spec");
    let schema = spec
        .response_schema("getNode")
        .expect("no error")
        .expect("schema");
    let query = parse("{ id label children { id children { id } } }").unwrap();
    assert!(validate(&query, &schema).is_empty());
}

const SPEC: &str = r##"{
  "openapi": "3.0.0",
  "paths": {
    "/users/{id}": {
      "get": {
        "operationId": "getUser",
        "summary": "Fetch a user",
        "responses": {
          "200": {
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/User" }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "User": {
        "type": "object",
        "required": ["id"],
        "properties": {
          "id": { "type": "integer" },
          "email": { "type": "string" },
          "profile": { "$ref": "#/components/schemas/Profile" }
        }
      },
      "Profile": {
        "type": "object",
        "properties": {
          "displayName": { "type": "string" }
        }
      }
    }
  }
}"##;

fn spec() -> Spec {
    Spec::parse(SPEC.as_bytes()).expect("spec should parse")
}

#[test]
fn lists_operations() {
    let ops = spec().operations();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].id, "getUser");
    assert_eq!(ops[0].method, "GET");
    assert_eq!(ops[0].path, "/users/{id}");
}

#[test]
fn resolves_refs_and_required_flags() {
    let schema = spec().response_schema("getUser").unwrap().expect("schema");
    let node = fields_at(&schema, &["profile", "displayName"]).unwrap();
    assert!(matches!(&node.kind, SchemaKind::Scalar(ScalarKind::String)));

    match &schema.kind {
        SchemaKind::Object(fields) => {
            assert!(fields.get("id").unwrap().required);
            assert!(!fields.get("email").unwrap().required);
        }
        other => panic!("expected object, got {other:?}"),
    }
}

#[test]
fn unknown_operation_is_not_an_error() {
    assert!(spec().response_schema("nope").unwrap().is_none());
}

#[test]
fn validates_good_query_against_spec() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let query = parse("{ id email profile { displayName } }").unwrap();
    assert!(validate(&query, &schema).is_empty());
}

#[test]
fn flags_typos_with_a_suggestion() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let query = parse("{ profil }").unwrap();
    let diags = validate(&query, &schema);
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message.contains("did you mean `profile`"),
        "{}",
        diags[0].message
    );
}

#[test]
fn reports_line_and_column_for_diagnostics() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let source = "{\n  id\n  profil\n}";
    let query = parse(source).unwrap();
    let diags = validate(&query, &schema);
    assert_eq!(diags[0].render(source).split(':').next(), Some("3"));
}

#[test]
fn rejects_selection_sets_on_scalars() {
    let schema = spec().response_schema("getUser").unwrap().unwrap();
    let query = parse("{ id { nested } }").unwrap();
    let diags = validate(&query, &schema);
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("cannot take a selection set"));
}
