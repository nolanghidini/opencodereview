use super::*;

pub(crate) fn route(request: Request, state: SharedState) -> Response {
    if request.method == "OPTIONS" {
        return json_ok(&serde_json::json!({ "ok": true }));
    }
    if request.method == "GET" && (request.path == "/" || request.path == "/onboarding") {
        return html_ok_owned(web_app_entry_html().unwrap_or_else(onboarding_html));
    }
    if request.method != "GET" && !has_json_content_type(&request) {
        return json_error(415, "Content-Type must be application/json");
    }
    if !is_public_route(&request.method, &request.path) {
        if let Some(response) = check_auth(&request, &state) {
            return response;
        }
    }
    let result = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => health(&state),
        ("GET", "/auth/github/status") => github_auth_status(),
        ("GET", "/auth/claude/status") => claude_auth_status(&state),
        ("POST", "/auth/claude/logout") => claude_auth_logout(&request, &state),
        ("POST", "/auth/github/logout") => github_auth_logout(&request),
        ("POST", "/auth/claude/start") => start_claude_auth(&request, &state),
        ("GET", "/auth/claude/events") => claude_auth_events(&request),
        ("POST", "/auth/claude/input") => claude_auth_input(&request),
        ("POST", "/auth/claude/cancel") => cancel_claude_auth(&request),
        ("POST", "/auth/claude/token") => save_claude_token(&request, &state),
        ("GET", "/ai/providers") => ai_providers(&state),
        ("GET", "/accounts/usage") => account_usage(&state),
        ("POST", "/auth/codex/start") => start_codex_auth(&request, &state),
        ("GET", "/auth/codex/events") => codex_auth_events(&request),
        ("POST", "/auth/codex/cancel") => cancel_codex_auth(&request),
        ("POST", "/auth/opencode/key") => save_opencode_key(&request, &state),
        ("GET", "/accounts") => list_accounts(&state),
        ("POST", "/accounts/order") => reorder_accounts(&request, &state),
        ("GET", "/review-engines") => review_engines(&state),
        ("POST", "/review-engines") => save_review_engine(&request, &state),
        ("POST", "/auth/github/start") => start_github_auth(&request, &state),
        ("POST", "/auth/github/cancel") => cancel_github_auth(&request, &state),
        ("GET", "/auth/github/events") => github_auth_events(&request, &state),
        ("GET", "/settings") => get_settings(&state),
        ("POST", "/settings") => save_setting(&request, &state),
        ("POST", "/settings/batch") => save_settings_batch_http(&request, &state),
        ("GET", "/github/workspaces") => refresh_github_workspaces(&state),
        ("GET", "/workspaces") => with_db_read(&state, |db| db.list_workspaces()),
        ("GET", "/indexing/jobs") => list_index_jobs(&state),
        ("GET", "/reviews/board") => list_review_board(&state),
        ("GET", "/usage") => usage_report_http(&request, &state),
        ("GET", "/usage/prs") => usage_prs_http(&request, &state),
        ("GET", "/pending") => with_db(&state, |db| {
            db.sync_review_attention_pendings()?;
            db.list_pending_actions()
        }),
        ("GET", "/skills/versions") => with_db_read(&state, |db| db.list_skill_versions()),
        ("POST", "/skills/versions") => create_skill_version_http(&request, &state),
        ("GET", "/reviews/poll/status") => with_db_read(&state, |db| db.poll_status()),
        ("GET", "/reviews/storage/status") => get_archive_storage_status(&state),
        ("POST", "/reviews/poll") => poll_reviews(&state),
        ("POST", "/workspaces/select") => select_workspaces(&request, &state),
        _ => route_dynamic(&request, &state),
    };
    match result {
        Ok(value) => json_ok(&value),
        Err(error) if error.starts_with("route not found") => json_error(404, &error),
        Err(error) if error.starts_with("forbidden") => json_error(403, &error),
        Err(error) => json_error(500, &error),
    }
}

pub(crate) fn route_dynamic(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    let parts: Vec<&str> = request
        .path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    match (request.method.as_str(), parts.as_slice()) {
        ("GET", ["workspaces", login, "repos"]) => list_workspace_repos(state, login),
        ("POST", ["workspaces", login, "repos", "refresh"]) => refresh_workspace_repos(state, login),
        ("POST", ["repos", repo_id, "subscribe"]) => subscribe_repo(request, state, parse_id(repo_id)?),
        ("POST", ["repos", repo_id, "index"]) => start_index(state, parse_id(repo_id)?),
        ("GET", ["repos", repo_id, "index"]) => with_db_read(state, |db| db.repo_index_status(parse_id(repo_id)?)),
        ("GET", ["repos", repo_id, "index", "graph"]) => with_db_read(state, |db| db.index_graph(parse_id(repo_id)?)),
        ("GET", ["repos", repo_id, "index", "symbol-graph"]) => {
            with_db_read(state, |db| db.symbol_graph(parse_id(repo_id)?))
        }
        ("GET", ["repos", repo_id, "index", "packages", package_id, "graph"]) => with_db_read(state, |db| {
            db.package_file_graph(parse_id(repo_id)?, parse_id(package_id)?)
        }),
        ("GET", ["repos", repo_id, "index", "progress"]) => index_progress(state, parse_id(repo_id)?),
        ("POST", ["repos", repo_id, "freshness"]) => check_freshness(state, parse_id(repo_id)?),
        ("GET", ["skills", "versions", id, "content"]) => skill_version_content_http(state, parse_id(id)?),
        ("POST", ["skills", "versions", id, "activate"]) => {
            require_auth(request, state)?;
            with_db(state, |db| {
                db.set_active_skill_version(parse_id(id)?)?;
                db.list_skill_versions()
            })
        }
        ("DELETE", ["skills", "versions", id]) => {
            require_auth(request, state)?;
            with_db(state, |db| db.delete_skill_version(parse_id(id)?))
        }
        ("POST", ["accounts", id]) => update_account(request, state, parse_id(id)?),
        ("DELETE", ["accounts", id]) => delete_account(request, state, parse_id(id)?),
        ("POST", ["reviews", pr_id, "start"]) => start_review_http(request, state, parse_id(pr_id)?),
        ("POST", ["reviews", pr_id, "cancel"]) => cancel_review_http(state, parse_id(pr_id)?),
        ("POST", ["reviews", pr_id, "decide"]) => decide_review_http(request, state, parse_id(pr_id)?),
        ("POST", ["reviews", pr_id, "result", "retry"]) => retry_review_result_http(state, parse_id(pr_id)?),
        ("GET", ["reviews", pr_id, "run"]) => with_db_read(state, |db| db.latest_review_run(parse_id(pr_id)?)),
        ("GET", ["reviews", pr_id, "runs"]) => with_db_read(state, |db| db.review_runs_for_pr(parse_id(pr_id)?)),
        ("GET", ["reviews", _pr_id, "runs", run_id, "stages"]) => {
            with_db_read(state, |db| db.review_stage_breakdown(parse_id(run_id)?))
        }
        ("GET", ["reviews", pr_id, "runs", run_id, "context"]) => with_db_read(state, |db| {
            let run_id = parse_id(run_id)?;
            let pr_id = parse_id(pr_id)?;
            db.review_run_by_id(run_id)?
                .filter(|run| run.pr_id == pr_id)
                .ok_or("Review run does not belong to this PR")?;
            db.context_plan_for_review_run(run_id)
        }),
        ("GET", ["reviews", pr_id, "runs", run_id, "findings"]) => {
            with_db_read(state, |db| db.finding_audit(parse_id(pr_id)?, parse_id(run_id)?))
        }
        ("POST", ["reviews", pr_id, "runs", run_id, "findings", "selection"]) => {
            let payload: FindingSelectionRequest =
                serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
            with_db(state, |db| {
                db.save_finding_selection(
                    parse_id(pr_id)?,
                    parse_id(run_id)?,
                    payload.revision,
                    &payload.selected_ids,
                )
            })
        }
        ("POST", ["reviews", pr_id, "seen"]) => mark_pr_review_seen_http(state, parse_id(pr_id)?),
        ("GET", ["reviews", pr_id, "context"]) => with_db_read(state, |db| db.latest_context_plan(parse_id(pr_id)?)),
        ("GET", ["reviews", pr_id, "threads"]) => with_db_read(state, |db| db.get_review_threads(parse_id(pr_id)?)),
        ("GET", ["reviews", pr_id, "usage"]) => with_db_read(state, |db| db.pr_usage(parse_id(pr_id)?)),
        ("GET", ["usage", "prs", pr_id, "runs"]) => usage_pr_runs_http(request, state, parse_id(pr_id)?),
        _ => Err(format!("route not found: {} {}", request.method, request.path)),
    }
}

pub(crate) fn health(state: &SharedState) -> Result<serde_json::Value, String> {
    let response = HealthResponse {
        ok: true,
        message: "OpenCodeReview server ready".to_string(),
        repos_dir: state.config.repos_dir.to_string_lossy().to_string(),
        github: github_status(),
        git: command_status("git", &["--version"]),
        claude: command_status("claude", &["--version"]),
    };
    serde_json::to_value(response).map_err(|error| error.to_string())
}

pub(crate) fn github_auth_status() -> Result<serde_json::Value, String> {
    let _ = crate::accounts::sandbox_sync::sync_app_auth();
    serde_json::to_value(github::auth_status()).map_err(|error| error.to_string())
}

pub(crate) fn github_auth_logout(request: &Request) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    github::sign_out()?;
    let _ = crate::accounts::sandbox_sync::sync_app_auth();
    Ok(serde_json::json!({ "ok": true }))
}

pub(crate) fn start_github_auth(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let payload: AuthStartRequest = if request.body.is_empty() {
        AuthStartRequest { action: None }
    } else {
        serde_json::from_slice(&request.body).map_err(|error| error.to_string())?
    };
    let action = if payload.action.as_deref() == Some("refresh") {
        "refresh"
    } else {
        "login"
    }
    .to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let already_running = {
        let mut slot = state
            .github_auth_cancel
            .lock()
            .map_err(|_| "github auth lock poisoned".to_string())?;
        if slot.is_some() {
            true
        } else {
            *slot = Some(cancel.clone());
            false
        }
    };
    if already_running {
        let flow_secret = state
            .github_auth_flow_secret
            .lock()
            .map_err(|_| "flow secret lock poisoned".to_string())?
            .clone();
        return Ok(serde_json::json!({ "running": true, "flowSecret": flow_secret }));
    }
    state
        .github_auth_events
        .lock()
        .map_err(|_| "github auth events lock poisoned".to_string())?
        .clear();
    let secret = generate_flow_secret();
    *state
        .github_auth_flow_secret
        .lock()
        .map_err(|_| "flow secret lock poisoned".to_string())? = Some(secret.clone());
    let events = state.github_auth_events.clone();
    let slot = state.github_auth_cancel.clone();
    thread::spawn(move || {
        let mut emit = |payload: serde_json::Value| {
            if let Ok(mut events) = events.lock() {
                events.push(payload);
            }
        };
        github::run_auth_flow(&cancel, &action, &mut emit);
        if let Ok(mut slot) = slot.lock() {
            *slot = None;
        }
    });
    Ok(serde_json::json!({ "running": true, "flowSecret": secret }))
}

pub(crate) fn cancel_github_auth(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    if let Some(cancel) = state
        .github_auth_cancel
        .lock()
        .map_err(|_| "github auth lock poisoned".to_string())?
        .take()
    {
        cancel.store(true, Ordering::Relaxed);
    }
    *state
        .github_auth_flow_secret
        .lock()
        .map_err(|_| "flow secret lock poisoned".to_string())? = None;
    Ok(serde_json::json!({ "running": false }))
}

pub(crate) fn github_auth_events(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let stored_secret = state
        .github_auth_flow_secret
        .lock()
        .map_err(|_| "flow secret lock poisoned".to_string())?
        .clone();
    if let Some(expected) = &stored_secret {
        match request.flow_secret.as_deref() {
            Some(provided) if provided == expected => {}
            _ => return Err("forbidden: invalid flow secret".to_string()),
        }
    }
    let running = state
        .github_auth_cancel
        .lock()
        .map_err(|_| "github auth lock poisoned".to_string())?
        .is_some();
    let events = state
        .github_auth_events
        .lock()
        .map_err(|_| "github auth events lock poisoned".to_string())?
        .clone();
    Ok(serde_json::json!({ "running": running, "events": events }))
}

pub(crate) fn get_settings(state: &SharedState) -> Result<serde_json::Value, String> {
    with_db_read(state, |db| db.settings())
}

pub(crate) fn json_body<T: DeserializeOwned>(request: &Request) -> Result<T, String> {
    serde_json::from_slice(&request.body).map_err(|error| error.to_string())
}

pub(crate) fn save_setting(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    let payload: SaveSettingRequest = serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
    let result = with_db(state, |db| db.save_setting(&payload.key, &payload.value))?;
    if payload.key == "review_archive_retention_days" {
        super::wake_retention_worker(state);
    }
    Ok(result)
}

pub(crate) fn save_settings_batch_http(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    let payload = json_body::<crate::db::SettingsUpdate>(request)?;
    let has_retention = payload.settings.contains_key("review_archive_retention_days");
    let result = with_db(state, |db| db.save_settings_batch(&payload))?;
    if has_retention {
        super::wake_retention_worker(state);
    }
    Ok(result)
}

pub(crate) fn get_archive_storage_status(state: &SharedState) -> Result<serde_json::Value, String> {
    // The last pass's status, which may have run on another instance.
    let stored = with_db_read_raw(state, |db| {
        db.setting(crate::review_archive::retention::STORAGE_STATUS_SETTING)
    })
    .ok()
    .flatten()
    .and_then(|raw| serde_json::from_str::<crate::review_archive::retention::ArchiveStorageStatus>(&raw).ok());
    if let Some(status) = stored {
        return serde_json::to_value(status).map_err(|e| e.to_string());
    }
    let status = state
        .archive_storage_status
        .lock()
        .map_err(|_| "Storage status unavailable".to_string())?;
    serde_json::to_value(&*status).map_err(|e| e.to_string())
}

pub(crate) fn refresh_github_workspaces(state: &SharedState) -> Result<serde_json::Value, String> {
    let workspaces = github::list_workspaces()?;
    with_db_raw(state, |db| db.upsert_workspaces(&workspaces))?;
    // One gh call per workspace; run them side by side so the slowest owner sets the wait.
    let listed: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = workspaces
            .iter()
            .map(|workspace| scope.spawn(move || github::list_repos(&workspace.login)))
            .collect();
        handles.into_iter().filter_map(|handle| handle.join().ok()).collect()
    });
    for repos in listed.into_iter().flatten() {
        with_db_raw(state, |db| db.upsert_repos(&repos))?;
    }
    with_db(state, |db| db.list_workspaces())
}
