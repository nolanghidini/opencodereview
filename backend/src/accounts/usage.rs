//! How much of each account's plan is left, read from the provider the way its own CLI does.
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const FRESH_FOR: Duration = Duration::from_secs(300);
static CACHE: Mutex<Option<HashMap<i64, (Instant, Value)>>> = Mutex::new(None);
/// Claude windows seen in review transcripts, for sign-ins whose usage endpoint is closed.
static OBSERVED: Mutex<Option<HashMap<i64, HashMap<i64, Value>>>> = Mutex::new(None);

/// `{ accountId, windows: [{ windowMins, usedPercent, resetsAt }], error? }`, cached for a few minutes.
pub(crate) fn usage(id: i64, provider: &str) -> Value {
    if let Some((at, value)) = CACHE.lock().ok().and_then(|cache| cache.as_ref()?.get(&id).cloned()) {
        if at.elapsed() < FRESH_FOR {
            return value;
        }
    }
    let windows = match provider {
        "claude" => claude(id).or_else(|error| {
            let seen = observed(id);
            if seen.is_empty() {
                Err(error)
            } else {
                Ok(seen)
            }
        }),
        "codex" => codex(id),
        "opencode" => Err("OpenCode doesn't share usage; check your OpenCode dashboard".to_string()),
        _ => Err("Unknown provider".to_string()),
    };
    let value = match windows {
        Ok(windows) => json!({ "accountId": id, "windows": windows }),
        Err(error) => json!({ "accountId": id, "windows": [], "error": error }),
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(HashMap::new)
            .insert(id, (Instant::now(), value.clone()));
    }
    value
}

pub(crate) fn forget(id: i64) {
    if let Ok(mut cache) = CACHE.lock() {
        if let Some(cache) = cache.as_mut() {
            cache.remove(&id);
        }
    }
    if let Ok(mut observed) = OBSERVED.lock() {
        if let Some(observed) = observed.as_mut() {
            observed.remove(&id);
        }
    }
}

/// Windows in a Claude stream-json transcript's `rate_limit_event`s, latest per window.
pub(crate) fn transcript_windows(transcript: &str) -> HashMap<i64, Value> {
    let mut windows = HashMap::new();
    for event in transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|event| event["type"] == "rate_limit_event")
    {
        let info = &event["rate_limit_info"];
        let Some(minutes) = (match info["rateLimitType"].as_str() {
            Some("five_hour") => Some(300),
            Some("seven_day") => Some(10_080),
            _ => None,
        }) else {
            continue;
        };
        let used = match info["utilization"].as_f64() {
            Some(fraction) if fraction <= 1.0 => fraction * 100.0,
            Some(percent) => percent,
            None if info["status"] == "rejected" => 100.0,
            None => continue,
        };
        let resets = info["resetsAt"]
            .as_i64()
            .map(|at| if at < 100_000_000_000 { at * 1000 } else { at });
        windows.insert(
            minutes,
            json!({ "windowMins": minutes, "usedPercent": used, "resetsAt": resets }),
        );
    }
    windows
}

/// Remembers what a finished Claude review reported about its account's windows.
pub(crate) fn observe_claude(id: i64, transcript: &str) {
    let windows = transcript_windows(transcript);
    if windows.is_empty() {
        return;
    }
    if let Ok(mut observed) = OBSERVED.lock() {
        observed
            .get_or_insert_with(HashMap::new)
            .entry(id)
            .or_default()
            .extend(windows);
    }
    if let Ok(mut cache) = CACHE.lock() {
        if let Some(cache) = cache.as_mut() {
            cache.remove(&id);
        }
    }
}

fn observed(id: i64) -> Vec<Value> {
    let mut windows = OBSERVED
        .lock()
        .ok()
        .and_then(|observed| observed.as_ref()?.get(&id).cloned())
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    windows.sort_by_key(|(minutes, _)| *minutes);
    windows.into_iter().map(|(_, window)| window).collect()
}

/// Claude's five-hour and weekly windows; `resets_at` stays an ISO-8601 string.
pub(crate) fn claude_windows(body: &Value) -> Vec<Value> {
    [("five_hour", 300), ("seven_day", 10_080)].iter().filter_map(|(key, minutes)| {
        let window = &body[key];
        Some(json!({ "windowMins": minutes, "usedPercent": window["utilization"].as_f64()?, "resetsAt": window["resets_at"].clone() }))
    }).collect()
}

/// Codex's primary and secondary windows; `resetsAt` arrives in seconds and leaves in milliseconds.
pub(crate) fn codex_windows(result: &Value) -> Vec<Value> {
    let limits = &result["rateLimits"];
    ["primary", "secondary"].iter().filter_map(|key| {
        let window = &limits[key];
        Some(json!({ "windowMins": window["windowDurationMins"].clone(), "usedPercent": window["usedPercent"].as_f64()?, "resetsAt": window["resetsAt"].as_i64().map(|seconds| seconds * 1000) }))
    }).collect()
}

fn claude(id: i64) -> Result<Vec<Value>, String> {
    let token = fs::read_to_string(crate::accounts::claude_token_path(id)).map_err(|_| "Not signed in")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token.trim())
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .map_err(|_| "Claude usage is unavailable right now")?;
    if response.status().is_success() {
        let body: Value = response.json().map_err(|_| "Claude returned unreadable usage")?;
        return Ok(claude_windows(&body));
    }
    // `claude setup-token` sign-ins can't read the usage endpoint, but every reply carries the windows.
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .bearer_auth(token.trim())
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "claude-haiku-5-5",
            "max_tokens": 1,
            "system": "You are Claude Code, Anthropic's official CLI for Claude.",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .map_err(|_| "Claude usage is unavailable right now")?;
    let windows = header_windows(|name| response.headers().get(name)?.to_str().ok().map(str::to_string));
    if windows.is_empty() {
        return Err("Claude shares this sign-in's usage after its next review".into());
    }
    Ok(windows)
}

/// Claude's `anthropic-ratelimit-unified-*` reply headers: utilization as a fraction, resets in epoch seconds.
pub(crate) fn header_windows(header: impl Fn(&str) -> Option<String>) -> Vec<Value> {
    [("5h", 300), ("7d", 10_080)].iter().filter_map(|(key, minutes)| {
        let used = header(&format!("anthropic-ratelimit-unified-{key}-utilization"))?.parse::<f64>().ok()?;
        let resets = header(&format!("anthropic-ratelimit-unified-{key}-reset")).and_then(|at| at.parse::<i64>().ok());
        Some(json!({ "windowMins": minutes, "usedPercent": used * 100.0, "resetsAt": resets.map(|seconds| seconds * 1000) }))
    }).collect()
}

/// Asks `codex app-server` in a private copy of the account's home, then keeps any refreshed sign-in.
fn codex(id: i64) -> Result<Vec<Value>, String> {
    let raw = fs::read_to_string(crate::accounts::codex_auth_path(id)).map_err(|_| "Not signed in")?;
    let home = std::env::temp_dir().join(format!("ocr-codex-usage-{}", crate::sandbox::secret()?));
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&home, fs::Permissions::from_mode(0o700));
    }
    let result = fs::write(home.join("auth.json"), &raw)
        .map_err(|error| error.to_string())
        .and_then(|()| codex_rate_limits(&home));
    if let Ok(refreshed) = fs::read_to_string(home.join("auth.json")) {
        if refreshed != raw && crate::providers::codex::validate_auth(&refreshed).is_ok() {
            let _ = crate::accounts::store_codex_auth(id, &refreshed);
        }
    }
    let _ = fs::remove_dir_all(&home);
    result
}

fn codex_rate_limits(home: &std::path::Path) -> Result<Vec<Value>, String> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .env("CODEX_HOME", home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Could not start the Codex CLI: {error}"))?;
    let stdout = child.stdout.take().ok_or("Codex output is unavailable")?;
    let (sender, replies) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] == json!(2) {
                let _ = sender.send(message);
                break;
            }
        }
    });
    let requests = [
        json!({ "method": "initialize", "id": 1, "params": { "clientInfo": { "name": "opencodereview", "title": null, "version": env!("CARGO_PKG_VERSION") }, "capabilities": null } }),
        json!({ "method": "initialized" }),
        json!({ "method": "account/rateLimits/read", "id": 2 }),
    ];
    let sent = child
        .stdin
        .as_mut()
        .ok_or("Codex input is unavailable")
        .and_then(|stdin| {
            requests
                .iter()
                .try_for_each(|request| writeln!(stdin, "{request}"))
                .map_err(|_| "Codex stopped before answering")
        });
    let reply = sent.map_err(str::to_string).and_then(|()| {
        replies
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "ChatGPT usage is unavailable right now".to_string())
    });
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    let reply = reply?;
    if let Some(message) = reply["error"]["message"].as_str() {
        return Err(format!("ChatGPT usage is unavailable: {message}"));
    }
    Ok(codex_windows(&reply["result"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_read_from_both_providers() {
        let claude = claude_windows(
            &json!({ "five_hour": { "utilization": 12.5, "resets_at": "2026-10-25T03:00:00Z" }, "seven_day": { "utilization": 40.0, "resets_at": null }, "seven_day_opus": null }),
        );
        assert_eq!(
            claude,
            vec![
                json!({ "windowMins": 300, "usedPercent": 12.5, "resetsAt": "2026-10-25T03:00:00Z" }),
                json!({ "windowMins": 10_080, "usedPercent": 40.0, "resetsAt": null }),
            ]
        );
        let codex = codex_windows(
            &json!({ "rateLimits": { "primary": { "usedPercent": 7, "windowDurationMins": 300, "resetsAt": 1_761_360_000 }, "secondary": null } }),
        );
        assert_eq!(
            codex,
            vec![json!({ "windowMins": 300, "usedPercent": 7.0, "resetsAt": 1_761_360_000_000_i64 })]
        );
    }

    #[test]
    fn claude_reply_headers_report_their_windows() {
        let headers = HashMap::from([
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1793124600"),
            ("anthropic-ratelimit-unified-7d-utilization", "bad"),
        ]);
        assert_eq!(
            header_windows(|name| headers.get(name).map(|value| value.to_string())),
            vec![json!({ "windowMins": 300, "usedPercent": 42.0, "resetsAt": 1_793_124_600_000_i64 })]
        );
    }

    #[test]
    fn claude_transcripts_report_their_latest_windows() {
        let transcript = [
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","utilization":0.2,"resetsAt":1761300000}}"#,
            r#"{"type":"assistant"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","utilization":0.81,"resetsAt":1761300000}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"seven_day","resetsAt":1761900000}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"overage"}}"#,
        ].join("\n");
        let windows = transcript_windows(&transcript);
        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows[&300],
            json!({ "windowMins": 300, "usedPercent": 81.0, "resetsAt": 1_761_300_000_000_i64 })
        );
        assert_eq!(windows[&10_080]["usedPercent"], json!(100.0));
        observe_claude(-7, &transcript);
        assert_eq!(observed(-7).len(), 2);
        forget(-7);
        assert!(observed(-7).is_empty());
    }
}
