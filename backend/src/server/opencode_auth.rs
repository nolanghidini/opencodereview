use super::*;

/// An OpenCode Go key, or no key for OpenCode's free models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OpencodeKey {
    key: Option<String>,
    model: Option<String>,
    account_id: Option<i64>,
}

/// OpenCode Go's model list answers 401 to a bad key and costs no tokens.
const GO_KEY_CHECK: &str = "https://opencode.ai/zen/go/v1/models";

/// Asks OpenCode whether it accepts `key` for OpenCode Go.
fn verify_key(key: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(GO_KEY_CHECK)
        .bearer_auth(key)
        .send()
        .map_err(|_| "Could not reach OpenCode to check the key; try again".to_string())?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err("OpenCode rejected that key".into()),
        status => Err(format!("OpenCode could not check the key (HTTP {status}); try again")),
    }
}

/// Saves an OpenCode account to `accountId` or to a new one: OpenCode Go once OpenCode accepts
/// the key, or the free models when no key is given.
pub(crate) fn save_opencode_key(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let input: OpencodeKey = serde_json::from_slice(&request.body).map_err(|_| "Invalid OpenCode request")?;
    let key = input.key.as_deref().map(str::trim).filter(|key| !key.is_empty());
    let model = input.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
    let raw = crate::providers::opencode::auth(if key.is_some() { "opencode-go" } else { "opencode" }, key, model)?;
    let (provider, _, _) = crate::providers::opencode::parts(&raw).ok_or("Choose OpenCode Go or the free models")?;
    if let Some(id) = input.account_id {
        if with_db_read_raw(state, |db| db.ai_account(id))?.provider != "opencode" {
            return Err("That account is not an OpenCode account".into());
        }
    }
    if let Some(key) = key {
        verify_key(key)?;
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
    fn the_key_check_goes_to_a_host_the_review_egress_allows() {
        let host = reqwest::Url::parse(GO_KEY_CHECK)
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        assert_eq!(host, "opencode.ai");
        let request: OpencodeKey = serde_json::from_str("{}").unwrap();
        assert!(request.key.is_none());
        assert!(serde_json::from_str::<OpencodeKey>(r#"{"provider":"anthropic","key":"x"}"#).is_err());
    }
}
