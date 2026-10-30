//! An applied query owns its bound literals. Later variable edits cannot change it.
use collection::send::Variables;
use response_query::Selection;

pub(crate) struct FrozenQuery {
    source: String,
    selection: Option<Selection>,
}

impl FrozenQuery {
    pub fn parse(
        source: &str,
        load: impl FnOnce() -> Result<Variables, String>,
    ) -> Result<Self, String> {
        let selection = if source.trim().is_empty() {
            None
        } else {
            Some(crate::query::parse(source, load)?)
        };
        Ok(Self {
            source: source.into(),
            selection,
        })
    }

    pub fn project(&self, body: &[u8]) -> Result<crate::ProjectionDto, String> {
        let started = std::time::Instant::now();
        let projected = self
            .selection
            .as_ref()
            .map(|selection| {
                response_query::project(body, selection).map_err(|diag| diag.render(&self.source))
            })
            .transpose()?;
        let bytes = projected.as_deref().unwrap_or(body);
        let (text, truncated, is_raw) = match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => (
                serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?,
                false,
                false,
            ),
            Err(_) => (
                String::from_utf8_lossy(&bytes[..bytes.len().min(crate::RAW_VIEW_LIMIT)])
                    .into_owned(),
                bytes.len() > crate::RAW_VIEW_LIMIT,
                true,
            ),
        };
        Ok(crate::ProjectionDto {
            view_id: 0,
            text,
            bytes: bytes.len(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            truncated,
            is_raw,
        })
    }

    pub fn export(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        let Some(selection) = &self.selection else {
            return Ok(body.to_vec());
        };
        let projected =
            response_query::project(body, selection).map_err(|diag| diag.render(&self.source))?;
        let value: serde_json::Value =
            serde_json::from_slice(&projected).map_err(|error| error.to_string())?;
        serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn an_applied_view_and_export_keep_the_original_bindings() {
        let body = br#"{"items":[{"id":"first"},{"id":"second"}]}"#;
        let source = r#"{ items[? id == "{{category}}"] { id } }"#;
        let first = FrozenQuery::parse(source, || {
            Ok(BTreeMap::from([("category".into(), "first".into())]).into())
        })
        .unwrap();
        let second = FrozenQuery::parse(source, || {
            Ok(BTreeMap::from([("category".into(), "second".into())]).into())
        })
        .unwrap();
        for (view, id) in [(&first, "first"), (&second, "second"), (&first, "first")] {
            let expected = serde_json::json!({"items":[{"id":id}]});
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&view.project(body).unwrap().text)
                    .unwrap(),
                expected
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&view.export(body).unwrap()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn empty_query_export_keeps_original_bytes_beyond_the_raw_display_limit() {
        let body = vec![b'x'; crate::RAW_VIEW_LIMIT + 10];
        let view = FrozenQuery::parse("", || panic!("no variables needed")).unwrap();
        assert!(view.project(&body).unwrap().truncated);
        assert_eq!(view.export(&body).unwrap(), body);
    }

    #[test]
    fn snapshot_ids_are_immutable_response_scoped_and_evicted_with_the_body() {
        let mut responses = crate::Responses::default();
        let response = responses.insert(br#"{"id":"first"}"#.to_vec());
        let first = responses
            .remember_view(
                response,
                FrozenQuery::parse(r#"{ id == "{{category}}" }"#, || {
                    Ok(BTreeMap::from([("category".into(), "first".into())]).into())
                })
                .unwrap(),
            )
            .unwrap();
        let second = responses
            .remember_view(
                response,
                FrozenQuery::parse(r#"{ id == "{{category}}" }"#, || {
                    Ok(BTreeMap::from([("category".into(), "second".into())]).into())
                })
                .unwrap(),
            )
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &responses.export_view(response, first).unwrap()
            )
            .unwrap(),
            serde_json::json!({"id":"first"})
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &responses.export_view(response, second).unwrap()
            )
            .unwrap(),
            serde_json::json!({})
        );
        assert!(responses.export_view(response + 1, first).is_err());
        for _ in 0..crate::MAX_KEPT_RESPONSES {
            responses.insert(b"{}".to_vec());
        }
        assert!(responses.views.is_empty());
        assert!(responses.export_view(response, first).is_err());
    }
}
