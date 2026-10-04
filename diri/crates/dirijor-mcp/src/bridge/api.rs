//! `open_api_request`: put an HTTP request in the caller's API tab.
//!
//! The agent describes the request; the app shows it, prefilled, in the
//! right panel of the agent's own Session. The bridge and Engine never send
//! it. The app sends only a `GET` the agent marked `auto_send`; any other
//! method waits for the person to press Send.

use diri_proto::{
    ApiEnvironmentDraft, ApiHeaderDraft, ApiRequestDraft, ApiVariableDraft, Method,
    SessionOpenApiRequestParams,
};
use serde_json::{Value, json};

use super::{Bridge, DEFAULT_TIMEOUT, McpPolicy, WriteAction};

impl Bridge {
    pub(super) fn open_api_request(&self, arguments: &Value) -> Result<Value, String> {
        let draft = api_request_draft(arguments)?;
        let snapshot = self.snapshot()?;
        McpPolicy::new(
            &snapshot.sessions,
            &snapshot.projects,
            self.caller.as_deref(),
        )?
        .authorize(WriteAction::ApiRequest)?;
        let caller = self.caller.clone().expect("policy requires a caller");
        let auto_send = draft.may_auto_send();
        let params = SessionOpenApiRequestParams {
            session_id: diri_proto::SessionId(caller.clone()),
            request: draft,
        };
        self.request(
            Method::SESSION_OPEN_API_REQUEST,
            serde_json::to_value(&params).map_err(|error| error.to_string())?,
            DEFAULT_TIMEOUT,
        )?;
        Ok(json!({
            "opened": true,
            "session_id": caller,
            "method": params.request.method,
            "auto_send": auto_send,
            "note": if auto_send {
                "Opened in your session's API tab and sent; the person sees the response there."
            } else {
                "Opened in your session's API tab. The person presses Send to run it."
            },
        }))
    }
}

/// The draft an `open_api_request` call describes. `json` (any JSON value)
/// becomes an indented body with `Content-Type: application/json` unless the
/// headers set one; `body` is sent as written.
pub(super) fn api_request_draft(arguments: &Value) -> Result<ApiRequestDraft, String> {
    let url = arguments
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "missing required argument: url".to_owned())?
        .to_owned();
    let method = arguments
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase();
    let mut headers: Vec<ApiHeaderDraft> = match arguments.get("headers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, value)| match value {
                Value::String(value) => Ok(ApiHeaderDraft {
                    name: name.clone(),
                    value: value.clone(),
                }),
                Value::Number(_) | Value::Bool(_) => Ok(ApiHeaderDraft {
                    name: name.clone(),
                    value: value.to_string(),
                }),
                _ => Err(format!("header {name} must be a string")),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("headers must be an object of name: value".into()),
    };
    let body = match (arguments.get("body"), arguments.get("json")) {
        (Some(_), Some(_)) => return Err("pass either body or json, not both".into()),
        (Some(Value::String(body)), None) => Some(body.clone()),
        (Some(_), None) => return Err("body must be a string; use json for a JSON value".into()),
        (None, Some(value)) => {
            if !headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case("content-type"))
            {
                headers.push(ApiHeaderDraft {
                    name: "Content-Type".into(),
                    value: "application/json".into(),
                });
            }
            Some(serde_json::to_string_pretty(value).map_err(|error| error.to_string())?)
        }
        (None, None) => None,
    };
    let environment = match arguments.get("variables") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => {
            let secrets: Vec<String> = arguments
                .get("secrets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let variables = map
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Value::String(value) => value.clone(),
                        Value::Number(_) | Value::Bool(_) => value.to_string(),
                        _ => return Err(format!("variable {name} must be a string")),
                    };
                    Ok(ApiVariableDraft {
                        name: name.clone(),
                        value,
                        secret: secrets.contains(name),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Some(ApiEnvironmentDraft {
                name: arguments
                    .get("environment")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                variables,
            })
        }
        Some(_) => return Err("variables must be an object of name: value".into()),
    };
    let draft = ApiRequestDraft {
        method,
        url,
        headers,
        body,
        name: arguments
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        environment,
        auto_send: arguments
            .get("auto_send")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    draft.validate()?;
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::audit_tests::Peer;

    #[test]
    fn a_json_value_becomes_an_indented_body_with_its_content_type() {
        let draft = api_request_draft(&json!({
            "method": "POST",
            "url": "{{baseUrl}}/items",
            "json": {"name": "tea", "qty": 2},
            "variables": {"baseUrl": "http://localhost:3000", "token": "sk_test"},
            "secrets": ["token"],
            "environment": "Local",
        }))
        .unwrap();
        assert_eq!(draft.method, "POST");
        assert_eq!(
            draft.body.as_deref(),
            Some("{\n  \"name\": \"tea\",\n  \"qty\": 2\n}")
        );
        assert_eq!(draft.headers[0].name, "Content-Type");
        let environment = draft.environment.unwrap();
        assert_eq!(environment.name.as_deref(), Some("Local"));
        let token = environment
            .variables
            .iter()
            .find(|variable| variable.name == "token")
            .unwrap();
        assert!(token.secret);
        assert!(
            !environment
                .variables
                .iter()
                .find(|variable| variable.name == "baseUrl")
                .unwrap()
                .secret
        );
    }

    #[test]
    fn only_get_keeps_auto_send() {
        let get =
            api_request_draft(&json!({"url": "http://localhost:3000/health", "auto_send": true}))
                .unwrap();
        assert_eq!(get.method, "GET", "GET is the default method");
        assert!(get.may_auto_send());
        let delete = api_request_draft(
            &json!({"method": "DELETE", "url": "http://localhost:3000/items/1", "auto_send": true}),
        )
        .unwrap();
        assert!(!delete.may_auto_send(), "a DELETE waits for Send");
    }

    #[test]
    fn malformed_requests_are_refused_before_the_engine() {
        for arguments in [
            json!({}),
            json!({"url": "http://x", "method": "TRACE"}),
            json!({"url": "ftp://x"}),
            json!({"url": "http://x", "headers": {"Bad Name": "1"}}),
            json!({"url": "http://x", "headers": {"X": "a\r\nInjected: 1"}}),
            json!({"url": "http://x", "body": "a", "json": {}}),
            json!({"url": "http://x", "variables": {"has space": "1"}}),
        ] {
            assert!(api_request_draft(&arguments).is_err(), "{arguments}");
        }
    }

    #[test]
    fn the_tool_schema_accepts_what_the_draft_reads() {
        let arguments = json!({
            "method": "PATCH",
            "url": "http://localhost:8080/v1/me",
            "headers": {"Authorization": "Bearer {{token}}"},
            "json": {"name": "Ada"},
            "name": "Rename me",
            "variables": {"token": "abc"},
            "secrets": ["token"],
            "environment": "Local",
            "auto_send": false,
        });
        crate::tools::validate_arguments("open_api_request", &arguments).unwrap();
        assert!(
            crate::tools::validate_arguments("open_api_request", &json!({"method": "GET"}))
                .is_err(),
            "url is required"
        );
        assert!(
            crate::tools::validate_arguments(
                "open_api_request",
                &json!({"url": "http://x", "bogus": 1})
            )
            .is_err()
        );
    }

    #[test]
    fn dispatch_relays_to_the_engine_for_the_calling_session() {
        let peer = Peer::new(|method| match method {
            Method::SESSION_LIST => json!({
                "sessions": [crate::bridge::tests::record("parent", None)],
                "projects": [],
            }),
            Method::SESSION_OPEN_API_REQUEST => json!({"opened": true, "autoSend": false}),
            other => panic!("unexpected {other}"),
        });
        let result = peer
            .bridge()
            .call(
                "open_api_request",
                &json!({"method": "POST", "url": "http://localhost:3000/items", "auto_send": true}),
            )
            .unwrap();
        assert_eq!(result["opened"], true);
        assert_eq!(result["session_id"], "parent");
        assert_eq!(result["auto_send"], false, "a POST never sends itself");
    }
}
