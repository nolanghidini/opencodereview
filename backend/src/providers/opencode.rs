//! OpenCode CLI execution for review stages that run on OpenCode's hosted models: the free ones,
//! or the paid OpenCode Go models with the user's key.
use serde_json::{json, Value};
use std::ffi::OsString;

use crate::providers::claude::Failure;

/// `auth.json` this app writes for an OpenCode account; anything larger is not one.
pub(crate) const MAX_AUTH_BYTES: usize = 16 * 1024;

/// What an OpenCode account reviews with.
pub(crate) struct KeyProvider {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    /// The variable OpenCode reads the key from; empty when the plan needs no key.
    pub(crate) env: &'static str,
    /// The model reviews use unless the account names one.
    pub(crate) model: &'static str,
}

/// OpenCode Go with the user's key, or OpenCode's free models with none. Both are served from
/// opencode.ai, the one host the review egress allows for OpenCode.
pub(crate) const PROVIDERS: &[KeyProvider] = &[
    KeyProvider {
        id: "opencode-go",
        name: "OpenCode Go",
        env: "OPENCODE_API_KEY",
        model: "opencode-go/glm-5.3",
    },
    KeyProvider {
        id: "opencode",
        name: "OpenCode free models",
        env: "",
        model: "opencode/big-pickle",
    },
];

pub(crate) fn provider(id: &str) -> Option<&'static KeyProvider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// Environment that keeps OpenCode to the configured provider: no model catalogue download,
/// updates, plugins, LSP downloads, sharing, project or Claude Code configuration.
pub(crate) const QUIET_ENV: &[(&str, &str)] = &[
    ("OPENCODE_DISABLE_MODELS_FETCH", "true"),
    ("OPENCODE_DISABLE_AUTOUPDATE", "true"),
    ("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true"),
    ("OPENCODE_DISABLE_LSP_DOWNLOAD", "true"),
    ("OPENCODE_DISABLE_SHARE", "true"),
    ("OPENCODE_DISABLE_PROJECT_CONFIG", "true"),
    ("OPENCODE_DISABLE_CLAUDE_CODE", "true"),
    ("OPENCODE_DISABLE_EXTERNAL_SKILLS", "true"),
    ("OPENCODE_DISABLE_AUTOCOMPACT", "true"),
];

/// Configuration for review stages: no web, subagents, todo lists, skills or file edits, no MCP
/// servers, no workspace snapshots. Shell reads stay allowed, as with Codex.
pub(crate) fn config(model: &str) -> Value {
    json!({
        "$schema": "https://opencode.ai/config.json",
        "autoupdate": false,
        "share": "disabled",
        "snapshot": false,
        "model": model,
        "mcp": {},
        "tools": { "webfetch": false, "websearch": false, "task": false, "todowrite": false, "todoread": false, "edit": false, "write": false, "patch": false, "skill": false },
        "permission": { "edit": "deny", "webfetch": "deny", "bash": "allow", "external_directory": "allow" },
    })
}

/// Arguments for one non-interactive execution: the prompt arrives on stdin and the events go
/// to stdout as JSON lines.
pub(crate) fn exec_args() -> Vec<OsString> {
    ["run", "--pure", "--format", "json", "--dir", "/work"]
        .map(OsString::from)
        .to_vec()
}

/// An OpenCode account: a plan, its key when the plan needs one, and an optional `provider/model`.
pub(crate) fn auth(provider: &str, key: Option<&str>, model: Option<&str>) -> Result<String, String> {
    let raw = json!({ "provider": provider, "key": key, "model": model }).to_string();
    validate_auth(&raw)?;
    Ok(raw)
}

/// Checks that `raw` names a supported plan, a plausible key when the plan needs one, and a
/// well-formed model of that plan.
pub(crate) fn validate_auth(raw: &str) -> Result<(), String> {
    if raw.len() > MAX_AUTH_BYTES {
        return Err("OpenCode authentication is too large".into());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "OpenCode authentication is not valid JSON")?;
    let provider = value["provider"]
        .as_str()
        .and_then(provider)
        .ok_or("Choose OpenCode Go or the free models")?;
    let key = value["key"].as_str().unwrap_or_default();
    if provider.env.is_empty() {
        if !key.is_empty() {
            return Err("The free models take no key".into());
        }
    } else if key.len() < 16 || key.len() > 512 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("Paste the whole OpenCode Go key".into());
    }
    if let Some(model) = value["model"].as_str() {
        let expected = format!("{}/", provider.id);
        if !model.starts_with(&expected)
            || model.len() > 200
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.:".contains(&byte))
        {
            return Err(format!("Write the model as {expected}model-name"));
        }
    }
    Ok(())
}

/// The plan, key (if any) and model of a validated `auth.json`; the model falls back to the
/// plan's default.
pub(crate) fn parts(raw: &str) -> Option<(&'static KeyProvider, Option<String>, String)> {
    validate_auth(raw).ok()?;
    let value: Value = serde_json::from_str(raw).ok()?;
    let provider = provider(value["provider"].as_str()?)?;
    let key = value["key"].as_str().filter(|key| !key.is_empty()).map(str::to_string);
    Some((
        provider,
        key,
        value["model"].as_str().unwrap_or(provider.model).to_string(),
    ))
}

/// The account's key, which must never appear in review output.
pub(crate) fn secrets(raw: &str) -> Vec<String> {
    parts(raw).and_then(|(_, key, _)| key).into_iter().collect()
}

/// The final answer: the text OpenCode printed after its last tool call.
pub(crate) fn final_message(transcript: &str) -> String {
    let mut message = String::new();
    for event in transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
    {
        match event["type"].as_str() {
            Some("tool_use") => message.clear(),
            Some("text") => {
                if !message.is_empty() {
                    message.push('\n');
                }
                message.push_str(event["part"]["text"].as_str().unwrap_or_default());
            }
            _ => {}
        }
    }
    message
}

/// The provider's own errors: OpenCode prints them as `error` events and still exits 0.
pub(crate) fn errors(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|event| event["type"] == "error")
        .map(|event| event["error"].clone())
        .collect()
}

/// Why an execution failed, read only from OpenCode's error events and stderr, never from tool
/// output, so reviewed code that mentions "quota" can't trip it.
pub(crate) fn classify(stdout: &str, stderr: &str) -> Failure {
    let errors = errors(stdout);
    let statuses = errors
        .iter()
        .filter_map(|error| error["data"]["statusCode"].as_i64())
        .collect::<Vec<_>>();
    let mut text = errors
        .iter()
        .map(|error| {
            format!(
                "{} {} {}",
                error["name"].as_str().unwrap_or_default(),
                error["data"]["message"].as_str().unwrap_or_default(),
                error["data"]["responseBody"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.push_str(stderr);
    let lower = text.to_ascii_lowercase();
    if [
        "insufficient_quota",
        "credit balance is too low",
        "exceeded your current quota",
        "insufficient credits",
        "billing",
    ]
    .iter()
    .any(|word| lower.contains(word))
        || statuses.contains(&402)
    {
        return Failure::UsageLimit { resets_at: None };
    }
    if statuses.iter().any(|status| matches!(status, 401 | 403))
        || [
            "invalid x-api-key",
            "invalid api key",
            "incorrect api key",
            "api key not valid",
            "authentication_error",
            "permission_denied",
        ]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Failure::Auth;
    }
    if statuses
        .iter()
        .any(|status| matches!(status, 429 | 500 | 502 | 503 | 529))
        || ["rate limit", "overloaded"].iter().any(|word| lower.contains(word))
    {
        return Failure::Overloaded;
    }
    if statuses.contains(&404)
        || (lower.contains("model")
            && (lower.contains("not found") || lower.contains("does not exist") || lower.contains("not supported")))
    {
        return Failure::Model;
    }
    Failure::Other
}

/// Short, safe diagnostic for a failed process; raw CLI output stays private.
pub(crate) fn failure(stdout: &str, stderr: &str) -> String {
    match classify(stdout, stderr) {
        Failure::Other if stdout.trim().is_empty() && stderr.trim().is_empty() => {
            "OpenCode process exited without diagnostics; verify runner health and the OpenCode Go key".into()
        }
        Failure::Other => "OpenCode execution failed; verify the OpenCode Go key and model".into(),
        Failure::Overloaded => "OpenCode is temporarily overloaded; retry later".into(),
        Failure::Model => "The chosen OpenCode model is unavailable".into(),
        failure => failure.encode(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_holds_a_go_key_or_nothing_for_the_free_models() {
        let raw = auth("opencode-go", Some("sk-opencode-fixture-key-000"), None).unwrap();
        let (provider, key, model) = parts(&raw).unwrap();
        assert_eq!(
            (provider.env, key.as_deref(), model.as_str()),
            (
                "OPENCODE_API_KEY",
                Some("sk-opencode-fixture-key-000"),
                "opencode-go/glm-5.3"
            )
        );
        assert_eq!(secrets(&raw), vec!["sk-opencode-fixture-key-000".to_string()]);
        let free = auth("opencode", None, None).unwrap();
        let (provider, key, model) = parts(&free).unwrap();
        assert_eq!((provider.env, key, model.as_str()), ("", None, "opencode/big-pickle"));
        assert!(secrets(&free).is_empty());
        assert!(auth("opencode", Some("sk-opencode-fixture-key-000"), None).is_err());
        assert!(auth("opencode-go", None, None).is_err());
        assert!(auth("opencode-go", Some("short"), None).is_err());
        assert!(auth("opencode-go", Some("sk-fixture key with spaces"), None).is_err());
        assert!(auth(
            "opencode-go",
            Some("sk-opencode-fixture-key-000"),
            Some("opencode/big-pickle")
        )
        .is_err());
        assert!(auth("anthropic", Some("sk-ant-api03-fixture-key-000"), None).is_err());
        assert_eq!(
            parts(
                &auth(
                    "opencode-go",
                    Some("sk-opencode-fixture-key-000"),
                    Some("opencode-go/kimi-k3")
                )
                .unwrap()
            )
            .unwrap()
            .2,
            "opencode-go/kimi-k3"
        );
    }

    #[test]
    fn config_and_args_keep_the_cli_to_reading() {
        let config = config("opencode-go/glm-5.3");
        assert_eq!(config["model"], "opencode-go/glm-5.3");
        for tool in ["webfetch", "websearch", "task", "edit", "write"] {
            assert_eq!(config["tools"][tool], false, "{tool}");
        }
        assert_eq!(config["permission"]["webfetch"], "deny");
        let args = exec_args();
        let args = args.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(args, ["run", "--pure", "--format", "json", "--dir", "/work"]);
        assert!(QUIET_ENV
            .iter()
            .any(|(name, _)| *name == "OPENCODE_DISABLE_PROJECT_CONFIG"));
    }

    #[test]
    fn the_final_message_is_the_text_after_the_last_tool_call() {
        let transcript = [
            r#"{"type":"step_start","part":{}}"#,
            r#"{"type":"text","part":{"type":"text","text":"Reading the diff"}}"#,
            r#"{"type":"tool_use","part":{"tool":"bash"}}"#,
            r#"{"type":"text","part":{"type":"text","text":"{\"report\":{}}"}}"#,
            r#"{"type":"step_finish","part":{"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":2,"write":0}}}}"#,
        ].join("\n");
        assert_eq!(final_message(&transcript), r#"{"report":{}}"#);
    }

    #[test]
    fn failures_come_from_error_events() {
        let event = |status: i64, message: &str| {
            format!(
                r#"{{"type":"error","error":{{"name":"APIError","data":{{"message":"{message}","statusCode":{status}}}}}}}"#
            )
        };
        assert_eq!(
            classify(r#"{"type":"tool_use","part":{"state":{"output":"401 quota"}}}"#, ""),
            Failure::Other
        );
        assert_eq!(classify(&event(401, "invalid x-api-key"), ""), Failure::Auth);
        assert_eq!(classify(&event(429, "Rate limit reached"), ""), Failure::Overloaded);
        assert_eq!(
            classify(
                &event(
                    429,
                    "You exceeded your current quota, please check your plan and billing details"
                ),
                ""
            ),
            Failure::UsageLimit { resets_at: None }
        );
        assert_eq!(
            classify(
                &event(400, "Your credit balance is too low to access the Anthropic API"),
                ""
            ),
            Failure::UsageLimit { resets_at: None }
        );
        assert_eq!(classify(&event(404, "model: claude-x not found"), ""), Failure::Model);
    }
}
