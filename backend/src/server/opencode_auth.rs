use super::*;

/// An OpenCode Go key, or no key for OpenCode's free models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OpencodeKey {
    key: Option<String>,
    model: Option<String>,
    account_id: Option<i64>,
}

/// The models OpenCode serves to a plan. The Go list answers 401 to a bad key and costs no tokens;
/// the free models are the ones Zen marks free.
fn models_url(plan: &str) -> &'static str {
    if plan == "opencode-go" {
        "https://opencode.ai/zen/go/v1/models"
    } else {
        "https://opencode.ai/zen/v1/models"
    }
}

/// The `plan/model` ids a model list names, sorted; for the free plan only the free models. Ids
/// an account could not store are left out.
fn parse_models(plan: &str, body: &str) -> Result<Vec<String>, String> {
    let unreadable = || "OpenCode sent a model list this app can't read".to_string();
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| unreadable())?;
    let mut models = value["data"]
        .as_array()
        .ok_or_else(unreadable)?
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| plan != "opencode" || id.ends_with("-free") || *id == "big-pickle")
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 120
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        })
        .map(|id| format!("{plan}/{id}"))
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("OpenCode offers no models to this plan".into());
    }
    Ok(models)
}

/// Asks OpenCode which models `plan` may use, which also checks the Go key.
fn list_models(plan: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.get(models_url(plan));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .map_err(|_| "Could not reach OpenCode to check the key; try again".to_string())?;
    match response.status().as_u16() {
        200..=299 => parse_models(
            plan,
            &response
                .text()
                .map_err(|_| "OpenCode's answer was cut off; try again")?,
        ),
        401 | 403 => Err("OpenCode rejected that key".into()),
        status => Err(format!("OpenCode could not check the key (HTTP {status}); try again")),
    }
}

/// Fails unless `id` is an OpenCode account.
fn require_opencode_account(state: &SharedState, id: i64) -> Result<(), String> {
    if with_db_read_raw(state, |db| db.ai_account(id))?.provider != "opencode" {
        return Err("That account is not an OpenCode account".into());
    }
    Ok(())
}

/// The plan, key and model stored for OpenCode account `id`.
fn stored_account(state: &SharedState, id: i64) -> Result<(&'static str, Option<String>, String), String> {
    require_opencode_account(state, id)?;
    let raw = std::fs::read_to_string(crate::accounts::opencode_auth_path(id))
        .map_err(|_| "Sign in to this OpenCode account again".to_string())?;
    let (provider, key, model) =
        crate::providers::opencode::parts(&raw).ok_or("Sign in to this OpenCode account again")?;
    Ok((provider.id, key, model))
}

/// Lists the models a pasted Go key, a stored account or the free plan may use, with the model the
/// account reviews with now, so the UI can offer them before anything is saved.
pub(crate) fn opencode_models(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let input: OpencodeKey = serde_json::from_slice(&request.body).map_err(|_| "Invalid OpenCode request")?;
    let key = input.key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let (plan, key, current) = match (key, input.account_id) {
        (Some(key), _) => ("opencode-go", Some(key.to_string()), None),
        (None, Some(id)) => {
            let (plan, key, model) = stored_account(state, id)?;
            (plan, key, Some(model))
        }
        (None, None) => ("opencode", None, None),
    };
    // Checks the key's shape before it leaves for OpenCode.
    crate::providers::opencode::auth(plan, key.as_deref(), None)?;
    let models = list_models(plan, key.as_deref())?;
    let default = crate::providers::opencode::provider(plan).map_or("", |provider| provider.model);
    Ok(serde_json::json!({
        "plan": plan,
        "current": current.unwrap_or_else(|| default.to_string()),
        "default": default,
        "models": models
            .iter()
            .map(|id| serde_json::json!({ "id": id, "label": id.split_once('/').map_or(id.as_str(), |(_, name)| name) }))
            .collect::<Vec<_>>(),
    }))
}

/// Saves an OpenCode account to `accountId` or to a new one: OpenCode Go once OpenCode accepts
/// the key, or the free models when no key is given. A Go model alone on a Go account changes
/// its model and keeps its key. A chosen model must be one OpenCode offers to the plan.
pub(crate) fn save_opencode_key(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let input: OpencodeKey = serde_json::from_slice(&request.body).map_err(|_| "Invalid OpenCode request")?;
    let key = input.key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let model = input.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
    if let Some(id) = input.account_id {
        require_opencode_account(state, id)?;
    }
    let (plan, key) = match (key, input.account_id, model) {
        (None, Some(id), Some(model)) if model.starts_with("opencode-go/") => {
            let (plan, key, _) = stored_account(state, id)?;
            if plan != "opencode-go" {
                return Err("Add an OpenCode Go key to use Go models".into());
            }
            (plan, key)
        }
        (Some(key), _, _) => ("opencode-go", Some(key.to_string())),
        (None, _, _) => ("opencode", None),
    };
    let raw = crate::providers::opencode::auth(plan, key.as_deref(), model)?;
    let (provider, _, _) = crate::providers::opencode::parts(&raw).ok_or("Choose OpenCode Go or the free models")?;
    if key.is_some() || model.is_some() {
        let models = list_models(plan, key.as_deref())?;
        if let Some(model) = model {
            if !models.iter().any(|id| id == model) {
                return Err(format!("OpenCode doesn't offer {model} to this plan"));
            }
        }
    }
    with_db_raw(state, |db| {
        let id = match input.account_id {
            Some(id) => id,
            None => {
                let count = db
                    .list_ai_accounts()?
                    .iter()
                    .filter(|account| account.provider == "opencode" && account.label.starts_with(provider.name))
                    .count();
                let label = if count == 0 {
                    provider.name.to_string()
                } else {
                    format!("{} {}", provider.name, count + 1)
                };
                db.create_ai_account("opencode", &label)?.id
            }
        };
        let stored = crate::accounts::store_opencode_auth(id, &raw);
        if stored.is_err() && input.account_id.is_none() {
            let _ = db.delete_ai_account(id);
        }
        stored?;
        db.reset_ai_account_status(id)
    })?;
    Ok(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_lists_come_from_a_host_the_review_egress_allows() {
        for plan in ["opencode-go", "opencode"] {
            let host = reqwest::Url::parse(models_url(plan))
                .unwrap()
                .host_str()
                .unwrap()
                .to_string();
            assert_eq!(host, "opencode.ai");
        }
        let request: OpencodeKey = serde_json::from_str("{}").unwrap();
        assert!(request.key.is_none());
        assert!(serde_json::from_str::<OpencodeKey>(r#"{"provider":"anthropic","key":"x"}"#).is_err());
    }

    #[test]
    fn model_lists_keep_the_plan_prefix_and_only_free_models_for_the_free_plan() {
        let body = r#"{"object":"list","data":[{"id":"glm-5.3"},{"id":"deepseek-v4.1-flash"},{"id":"glm-5.3"},{"id":"bad id"}]}"#;
        assert_eq!(
            parse_models("opencode-go", body).unwrap(),
            vec!["opencode-go/deepseek-v4.1-flash", "opencode-go/glm-5.3"]
        );
        let free = r#"{"data":[{"id":"big-pickle"},{"id":"claude-sonnet-5-5"},{"id":"mimo-v2.6-flash-free"}]}"#;
        assert_eq!(
            parse_models("opencode", free).unwrap(),
            vec!["opencode/big-pickle", "opencode/mimo-v2.6-flash-free"]
        );
        assert!(parse_models("opencode", r#"{"data":[{"id":"claude-sonnet-5-5"}]}"#).is_err());
        assert!(parse_models("opencode-go", "<html>").is_err());
        // Every listed model is one an account can store.
        for id in parse_models("opencode-go", body).unwrap() {
            assert!(crate::providers::opencode::auth("opencode-go", Some("k".repeat(32).as_str()), Some(&id)).is_ok());
        }
    }
}
