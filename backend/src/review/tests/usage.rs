use super::{collect_usage, finalize_usage};
use std::collections::HashMap;

#[test]
fn normalizes_model_labels_to_one_family() {
    use super::normalize_model;
    assert_eq!(normalize_model("claude-opus-5-5"), "claude-opus-5-5");
    assert_eq!(normalize_model("claude-opus-5-5-high"), "claude-opus-5-5");
    assert_eq!(normalize_model("claude-haiku-5-5-20260301"), "claude-haiku-5-5");
    assert_eq!(normalize_model("Claude Sonnet 5.5"), "claude-sonnet-5-5");
    assert_eq!(normalize_model(""), "unknown");
}

#[test]
fn attributes_usage_per_model_with_totals_and_dominant() {
    let transcript = serde_json::json!([
        {"type": "system", "subtype": "init", "model": "claude-opus-5-5"},
        {"type": "assistant", "message": {"usage": {"input_tokens": 999, "output_tokens": 999}}},
        {"type": "result", "subtype": "success", "modelUsage": {
            "claude-opus-5-5": {"inputTokens": 140, "outputTokens": 14, "cacheReadInputTokens": 70, "cacheCreationInputTokens": 30},
            "claude-haiku-5-5-20260301": {"inputTokens": 60, "outputTokens": 6}
        }}
    ]);
    let mut per_model = HashMap::new();
    collect_usage(&transcript, &mut per_model);
    let usage = finalize_usage(per_model);

    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.cache_read_tokens, 70);
    assert_eq!(usage.cache_creation_tokens, 30);
    assert_eq!(usage.by_model.len(), 2);
    assert_eq!(usage.model.as_deref(), Some("claude-opus-5-5"));
}

#[test]
fn falls_back_to_result_usage_and_parses_stream_lines() {
    let raw = "{\"type\":\"system\"}\n{\"type\":\"result\",\"usage\":{\"input_tokens\":30,\"output_tokens\":3,\"cache_read_input_tokens\":10}}\n";
    let events = super::transcript_events(raw).unwrap();
    let mut per_model = HashMap::new();
    collect_usage(&events, &mut per_model);
    let usage = finalize_usage(per_model);
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cache_read_tokens),
        (30, 3, 10)
    );
    assert_eq!(usage.model.as_deref(), Some("unknown"));
    assert!(super::transcript_events("not json\n").is_none());
    assert!(super::transcript_events("").is_none());
}

pub(super) fn review_inputs(depth: &str, threshold: usize, chunk_size: usize) -> super::ReviewInputs {
    super::ReviewInputs {
        frozen: None,
        run_id: 1,
        repo_id: 1,
        pr_id: 1,
        repo_path: std::path::PathBuf::from("/tmp/repo"),
        full_name: "owner/repo".to_string(),
        number: 1,
        base_ref: "main".to_string(),
        fixed_head_sha: None,
        offline: false,
        expected_scope: None,
        prior_scope: None,
        last_reviewed_sha: None,
        force_full: false,
        reprocess: false,
        skill_content: String::new(),
        unresolved_review_head: None,
        unresolved_review_body: None,
        pr_title: String::new(),
        pr_body: String::new(),
        model: "claude-opus-5-5".to_string(),
        review_depth_mode: depth.to_string(),
        review_deep_threshold: threshold,
        review_chunk_size: chunk_size,
        review_parallel_chunks: 1,
        db_handle: Some(crate::db::Db::for_test()),
        archive_dir: std::path::PathBuf::from("review-archives"),
        budget: None,
    }
}

#[test]
fn explicit_reprocess_disables_result_cache_but_keeps_scope_coverage() {
    let mut inputs = review_inputs("economic", 20, 5);
    assert!(super::should_reuse_chunk_results(&inputs));
    inputs.reprocess = true;
    assert!(!super::should_reuse_chunk_results(&inputs));
    assert!(super::should_reuse_review_coverage(&inputs));
    inputs.reprocess = false;
    inputs.force_full = true;
    assert!(!super::should_reuse_chunk_results(&inputs));
}

#[test]
fn force_full_reviews_bypass_persisted_coverage() {
    let mut inputs = review_inputs("economic", 20, 5);
    assert!(super::should_reuse_review_coverage(&inputs));
    inputs.force_full = true;
    assert!(!super::should_reuse_review_coverage(&inputs));
}

#[test]
fn review_context_uses_the_pinned_head_graph() {
    let root = super::make_run_dir(-782).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("package.json"), r#"{"name":"fixture"}"#).unwrap();
    std::fs::write(repo.join("entry.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(repo.join("dep.ts"), "export const value = 2;\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(
        repo.join("entry.ts"),
        "import { value } from './dep';\nexport const result = value;\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "head"]);
    let head = git(&["rev-parse", "HEAD"]);
    let mut inputs = review_inputs("auto", 20, 5);
    inputs.repo_path = repo.clone();
    inputs.base_ref = base;
    let (plan, indexed) = super::build_snapshot_context_plan(
        &inputs,
        &["entry.ts".into()],
        &head,
        &inputs.base_ref,
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(indexed.as_deref(), Some(head.as_str()));
    assert!(plan.dependency_edges.contains(&("entry.ts".into(), "dep.ts".into())));
    assert!(plan
        .related_files
        .iter()
        .any(|file| file.path == "dep.ts" && file.source == "head"));
    assert_eq!(git(&["rev-parse", "HEAD"]), head);
    git(&["remote", "add", "origin", repo.to_str().unwrap()]);
    git(&["branch", "deadbeef", &head]);
    assert_eq!(
        super::resolve_base_rev(&repo, "deadbeef", &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        head
    );
    git(&["branch", "parent", &inputs.base_ref]);
    let parent = super::resolve_base_rev(&repo, "parent", &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(parent, inputs.base_ref);
    git(&["update-ref", "refs/remotes/origin/main", &head]);
    git(&["update-ref", "-d", "refs/heads/parent"]);
    assert!(super::rev_exists(
        &repo,
        "origin/parent",
        &std::sync::atomic::AtomicBool::new(false)
    ));
    let error = super::resolve_base_rev(&repo, "parent", &std::sync::atomic::AtomicBool::new(false)).unwrap_err();
    assert!(error.contains("Refusing to use a stale ref"));
    git(&[
        "remote",
        "set-url",
        "origin",
        root.join("missing-remote").to_str().unwrap(),
    ]);
    assert!(super::resolve_base_rev(&repo, "main", &std::sync::atomic::AtomicBool::new(false)).is_err());
    assert_eq!(git(&["rev-parse", "HEAD"]), head);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn single_review_keeps_structured_evidence_for_any_selected_model() {
    let mut inputs = review_inputs("economic", 20, 5);
    inputs.model = "custom-review-model".into();
    let prompt = super::render_prompt(
        &inputs,
        std::path::Path::new("/context"),
        std::path::Path::new("/run/summary.md"),
        &["src/api.rs".into()],
    );
    assert!(prompt.contains("/run/report.json"));
    assert!(prompt.contains("complete review under the report key"));
    assert!(prompt.contains("Do not write output files"));
    let example = prompt
        .split("Response schema (all report fields required): ")
        .nth(1)
        .unwrap()
        .split(". kind is")
        .next()
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(example).unwrap();
    assert!(response["report"]["reviewedFiles"].is_array());
    assert!(prompt.contains("reviewedFiles"));
    assert!(prompt.contains("counterEvidence"));
    assert_eq!(inputs.model, "custom-review-model");
}

#[test]
fn chunk_planning_failure_records_zero_inference_without_claiming_success() {
    let phases: serde_json::Value = serde_json::from_str(&super::chunk_planning_failure_phase()).unwrap();
    let chunks = phases
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["id"] == "chunks")
        .unwrap();
    assert_eq!(chunks["status"], "failed");
    assert_eq!(chunks["inferenceSkipped"], true);
    assert_eq!(chunks["chunks"], serde_json::json!([]));
    let final_phase = phases
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["id"] == "final")
        .unwrap();
    assert_eq!(final_phase["status"], "pending");
    assert_eq!(final_phase["inferenceSkipped"], true);
}

#[test]
fn no_changes_marks_inference_as_skipped_not_executed() {
    let phases: serde_json::Value = serde_json::from_str(&super::no_change_phase_json()).unwrap();
    for phase in phases
        .as_array()
        .unwrap()
        .iter()
        .filter(|phase| phase["id"] == "review" || phase["id"] == "validate")
    {
        assert_eq!(phase["status"], "skipped");
        assert_eq!(phase["inferenceSkipped"], true);
    }
}

#[test]
fn economic_review_does_not_force_an_extra_pass() {
    let inputs = review_inputs("economic", 20, 5);
    for files in [0, 1, 20] {
        assert!(!super::should_run_sharded_review(&inputs, files));
    }
}

#[test]
fn missing_output_is_not_a_clean_review() {
    for raw in ["", "{}", "{\"commit_id\":\"head\"}"] {
        assert!(super::require_valid_comments(raw, "head", "").is_err());
    }
    assert!(super::require_valid_comments(r#"{"commit_id":"head","comments":[]}"#, "head", "").is_ok());
}

#[test]
fn distinct_findings_at_one_anchor_are_not_collapsed_by_attribution() {
    let diff =
        "diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -0,0 +1 @@\n+allow_all();\n";
    let body = |title: &str| {
        format!("<sub>🤖 Automated review by OpenCodeReview</sub>\n\n**[Finding] {title}**\n\n🔎 Details\n\nDetails\n\n🛠️ Suggested fix\n\nFix\n\n🤖 Prompt for AI Agents\n\n```text\n{preamble}\n```", preamble = super::FINDING_PROMPT_PREAMBLE)
    };
    let comments = serde_json::json!({"commit_id":"head","comments":[
        {"path":"src/auth.rs","line":1,"side":"RIGHT","body":body("Tenant boundary")},
        {"path":"src/auth.rs","line":1,"side":"RIGHT","body":body("Audit trail")}
    ]});
    let validated = super::require_valid_comments(&comments.to_string(), "head", diff).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&validated).unwrap()["comments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let duplicate = serde_json::json!({"commit_id":"head","comments":[
        {"path":"src/auth.rs","line":1,"side":"RIGHT","body":body("Tenant boundary")},
        {"path":"src/auth.rs","line":1,"side":"RIGHT","body":body("Tenant boundary")}
    ]});
    let error = super::require_valid_comments(&duplicate.to_string(), "head", diff).unwrap_err();
    assert!(error.contains("comments[1]: duplicate inline comment"));
    assert!(!error.contains("Tenant boundary"));
    let mut outside = comments;
    outside["comments"][1]["path"] = serde_json::json!("untrusted-model-path");
    let error = super::require_valid_comments(&outside.to_string(), "head", diff).unwrap_err();
    assert!(error.contains("comments[1]: anchor outside validation diff"));
    assert!(error.contains("Report hash:"));
    assert!(!error.contains("untrusted-model-path") && !error.contains("Audit trail"));
}

#[test]
fn incremental_publication_scope_requires_matching_coordinates_and_text() {
    let full =
        "diff --git a/code.rs b/code.rs\n--- a/code.rs\n+++ b/code.rs\n@@ -1 +1,2 @@\n-base();\n+prior();\n+next();\n";
    let append = "diff --git a/code.rs b/code.rs\n--- a/code.rs\n+++ b/code.rs\n@@ -1 +1,2 @@\n prior();\n+next();\n";
    assert!(super::incremental_diff_is_publishable(append, full));
    assert!(super::incremental_diff_is_publishable(full, full));
    assert!(super::incremental_diff_is_publishable("", full));
    let changed_prior =
        "diff --git a/code.rs b/code.rs\n--- a/code.rs\n+++ b/code.rs\n@@ -1 +1,2 @@\n-prior();\n+prior();\n+next();\n";
    assert_eq!(super::diff_anchors(changed_prior), super::diff_anchors(full));
    assert!(!super::incremental_diff_is_publishable(changed_prior, full));
    let shifted_left = full.replace("@@ -1 +1,2 @@", "@@ -2 +1,2 @@");
    assert!(!super::incremental_diff_is_publishable(&shifted_left, full));
    let reverted_right = append.replace("+next();", "+base();");
    assert!(!super::incremental_diff_is_publishable(&reverted_right, full));
    assert!(!super::incremental_diff_is_publishable(
        &append.replace("+1,2", "+3,2"),
        full
    ));
}

#[test]
#[ignore = "Replays retained incremental/full diff anchor mismatches offline; no model calls"]
fn retained_incremental_comments_fail_full_diff_validation() {
    let path = std::env::var("OCR_DIAGNOSTIC_SCOPE_DIFFS").unwrap();
    let packet: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let head = packet["head"].as_str().unwrap();
    let active = packet["activeDiff"].as_str().unwrap();
    let full = packet["fullDiff"].as_str().unwrap();
    let active_anchors = super::diff_anchors(active);
    let full_anchors = super::diff_anchors(full);
    let mut incompatible = active_anchors.difference(&full_anchors).collect::<Vec<_>>();
    incompatible.sort();
    assert!(!incompatible.is_empty());
    assert!(!super::incremental_diff_is_publishable(active, full));
    assert!(super::incremental_diff_is_publishable(full, full));
    let body = format!("**[Finding] Offline anchor regression**\n\n\u{1f50e} Details\n\nSynthetic validation fixture.\n\n\u{1f6e0}\u{fe0f} Suggested fix\n\nKeep publication scope consistent.\n\n\u{1f916} Prompt for AI Agents\n\n```text\n{}\n```", super::FINDING_PROMPT_PREAMBLE);
    for (path, line, side) in &incompatible {
        let raw = serde_json::json!({"commit_id":head,"comments":[{"path":path,"line":line,"side":side,"body":body}]})
            .to_string();
        super::require_valid_comments(&raw, head, active).unwrap();
        assert!(super::require_valid_comments(&raw, head, full)
            .unwrap_err()
            .contains("cannot be silently discarded"));
    }
    println!(
        "REPRODUCED_PUBLICATION_SCOPE {}",
        serde_json::json!({"runId":packet["runId"],"incompatibleAnchors":incompatible.len(),"modelCalls":0})
    );
}

#[test]
fn deleted_file_anchors_use_the_old_path() {
    let diff = "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-old();\n";
    assert!(super::diff_anchors(diff).contains(&("old.rs".into(), 1, "LEFT".into())));
}

#[test]
fn context_cache_ignores_temp_roots_but_not_evidence_changes() {
    let root = super::make_run_dir(-781).unwrap();
    let a = root.join("a");
    let b = root.join("b");
    for dir in [&a, &b] {
        super::write_under(dir, "related/caller.rs", "guard();\n");
    }
    let first = super::context_fingerprint(&a).unwrap();
    assert_eq!(first, super::context_fingerprint(&b).unwrap());
    super::write_under(&b, "related/caller.rs", "unguarded();\n");
    assert_ne!(first, super::context_fingerprint(&b).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_sharding_uses_current_review_scope() {
    let inputs = review_inputs("auto", 20, super::DEFAULT_REVIEW_CHUNK_SIZE);

    assert!(!super::should_run_sharded_review(&inputs, 3));
    assert!(super::should_run_sharded_review(&inputs, 20));
}

#[test]
fn deep_does_not_shard_single_chunk_scope() {
    let inputs = review_inputs("deep", 20, super::DEFAULT_REVIEW_CHUNK_SIZE);

    assert!(!super::should_run_sharded_review(
        &inputs,
        super::DEFAULT_REVIEW_CHUNK_SIZE
    ));
    assert!(super::should_run_sharded_review(
        &inputs,
        super::DEFAULT_REVIEW_CHUNK_SIZE + 1
    ));
}

#[test]
fn sharded_chunks_cover_reviewable_files_with_max_size() {
    let map = super::ReviewMap {
        dependency_edges: Vec::new(),
        pr_number: 7,
        title: "title".to_string(),
        description: String::new(),
        head_sha: "abc".to_string(),
        base_ref: "main".to_string(),
        changed_files: vec![],
        reviewable_files: (0..12).map(|i| format!("src/file{i}.rs")).collect(),
        omitted_noise_files: vec![],
        diff_stats: super::DiffStats {
            added: 1,
            removed: 0,
            files: 12,
        },
        categories: vec![],
        packages: vec![],
        dependent_packages: vec![],
        related_files: vec![],
        convention_files: vec![],
        http_refs: vec![],
        risk_triggers: vec![],
        data_lifecycle_triggers: vec![],
        public_contract_triggers: vec![],
        test_relationship_hints: vec![],
    };
    let chunks = super::build_review_chunks(&map, super::DEFAULT_REVIEW_CHUNK_SIZE);
    let files: Vec<_> = chunks.iter().flat_map(|chunk| chunk.files.clone()).collect();

    assert_eq!(chunks.len(), 3);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.files.len() <= super::DEFAULT_REVIEW_CHUNK_SIZE));
    assert_eq!(files.len(), map.reviewable_files.len());
    assert!(map.reviewable_files.iter().all(|file| files.contains(file)));
}

pub(super) fn graph_chunk_fixture(
    id: i64,
    skill_bytes: usize,
    file_bytes: usize,
) -> (std::path::PathBuf, super::ReviewMap) {
    let root = super::make_run_dir(id).unwrap();
    std::fs::create_dir_all(root.join("changed")).unwrap();
    std::fs::create_dir_all(root.join("skill")).unwrap();
    std::fs::write(root.join("skill/SKILL.md"), "s".repeat(skill_bytes)).unwrap();
    std::fs::write(root.join("PR_DIFF.patch"), "").unwrap();
    let files = vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()];
    for path in &files {
        std::fs::write(root.join("changed").join(path), "x".repeat(file_bytes)).unwrap();
    }
    let inputs = review_inputs("deep", 10, 5);
    let plan = crate::db::ReviewContextPlan {
        repo_id: 1,
        base_ref: None,
        base_sha: None,
        changed_files: files.clone(),
        reviewed_base_sha: None,
        merge_base_sha: None,
        scope_reason: None,
        scope_selection: None,
        dependency_edges: vec![],
        packages: vec![],
        dependent_packages: vec![],
        related_files: vec![],
        test_files: vec![],
        convention_files: vec![],
        omitted_count: 0,
        incremental: false,
        range: String::new(),
        head_sha: None,
        prior_sha: None,
    };
    let map = super::build_review_map(&inputs, &plan, "head", &files, "");
    super::write_review_map(&root, &map).unwrap();
    (root, map)
}

pub(super) fn fixture_chunk_prompt_bytes(root: &std::path::Path, chunk: &super::ReviewChunk) -> usize {
    super::planned_chunk_prompt(&review_inputs("deep", 20, 5), root, chunk, "head", None).len()
}

#[test]
fn chunk_metadata_does_not_charge_other_files_http_refs() {
    let (root, mut map) = graph_chunk_fixture(-987, 90_000, 2_616);
    let other = "apps/fleet-triage/tests/case-detail/handlers/case-detail.ts";
    map.http_refs = (0..321)
        .map(|_| super::HttpRef {
            path: other.into(),
            method: "GET".into(),
            route: "unknown".into(),
            side: "frontend".into(),
        })
        .collect();
    map.http_refs.push(super::HttpRef {
        path: "a.rs".into(),
        method: "GET".into(),
        route: "/cases".into(),
        side: "backend".into(),
    });
    map.categories.push(super::FileCategory {
        path: other.into(),
        category: "test".into(),
    });
    map.risk_triggers = vec!["runtime/config: a.rs".into(), format!("runtime/config: {other}")];
    map.public_contract_triggers = vec!["a.rs".into(), other.into()];
    map.data_lifecycle_triggers = map.public_contract_triggers.clone();
    map.test_relationship_hints = vec!["b.rs may test a.rs".into(), format!("{other} may test c.rs")];
    super::write_review_map(&root, &map).unwrap();
    assert!(super::dir_bytes(&root) > crate::review::policy::TARGET_CONTEXT_BYTES as i64);
    let chunk = super::ReviewChunk {
        id: 1,
        files: vec!["a.rs".into()],
        related_files: vec!["b.rs".into()],
        global_context_files: vec![],
        omitted_convention_files: vec![],
        risk_triggers: vec![],
        cross_chunk_group_ids: vec![],
    };
    let scoped = super::chunk_review_map(&map, &chunk);
    assert_eq!(scoped.http_refs.len(), 1);
    assert_eq!(scoped.http_refs[0].path, "a.rs");
    assert!(scoped
        .categories
        .iter()
        .all(|category| category.path == "a.rs" || category.path == "b.rs"));
    assert_eq!(scoped.risk_triggers, ["runtime/config: a.rs"]);
    assert_eq!(scoped.public_contract_triggers, ["a.rs"]);
    assert_eq!(scoped.data_lifecycle_triggers, ["a.rs"]);
    assert_eq!(scoped.test_relationship_hints, ["b.rs may test a.rs"]);
    assert_eq!(scoped.reviewable_files, map.reviewable_files);
    assert!(
        super::chunk_context_bytes(
            &map,
            &root,
            &chunk,
            &(std::collections::BTreeMap::new(), 0),
            fixture_chunk_prompt_bytes(&root, &chunk)
        )
        .unwrap()
            < crate::review::policy::TARGET_CONTEXT_BYTES
    );
    assert_eq!(map.http_refs.len(), 322);
    assert_eq!(std::fs::read(root.join("changed/a.rs")).unwrap().len(), 2_616);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_refs_require_method_tokens_and_literal_routes_and_are_unique() {
    let diff = "diff --git a/src/view.tsx b/src/view.tsx\n--- a/src/view.tsx\n+++ b/src/view.tsx\n+screen.getByText(\"/not-a-route\");\n+const output = \"/not-an-endpoint\";\n+const target = \"/not-a-request\";\n+api.get(\"/cases\");\n+api.get(\"/cases\");\n+request({ method: \"PATCH\", url: \"/cases/:id\" });\n+const method = \"GET\";\n";
    let refs = super::extract_http_refs(diff);
    assert_eq!(refs.len(), 2);
    assert_eq!((refs[0].method.as_str(), refs[0].route.as_str()), ("GET", "/cases"));
    assert_eq!(
        (refs[1].method.as_str(), refs[1].route.as_str()),
        ("PATCH", "/cases/:id")
    );
    assert!(refs.iter().all(|reference| reference.path == "src/view.tsx"));
}

fn diagnostic_run_is_eligible(status: &str, finished: bool, explicit: bool, reprocess: bool) -> bool {
    finished && (status == "failed" || (explicit && (status == "cancelled" || reprocess)))
}

#[test]
fn diagnostic_hydration_and_export_share_finished_run_eligibility() {
    for status in ["failed", "cancelled", "posted", "running"] {
        for finished in [false, true] {
            for explicit in [false, true] {
                for reprocess in [false, true] {
                    let expected = finished && (status == "failed" || explicit && (status == "cancelled" || reprocess));
                    assert_eq!(
                        diagnostic_run_is_eligible(status, finished, explicit, reprocess),
                        expected
                    );
                }
            }
        }
    }
}

pub(crate) struct RecordedDependencies {
    repositories: std::collections::BTreeSet<String>,
    provenance: Vec<crate::review_archive::DependencyProvenance>,
}

pub(crate) fn load_recorded_dependencies(
    archives_dir: &std::path::Path,
    db: &crate::db::Db,
    run_id: i64,
) -> Result<Option<RecordedDependencies>, crate::review_archive::Error> {
    match std::fs::symlink_metadata(archives_dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(crate::review_archive::Error::Integrity);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(crate::review_archive::Error::Preparation),
    }
    let store = crate::review_archive::ArchiveStore::open_read(archives_dir, db)?;
    let archive = store.load_linked(&format!("review:{run_id}"))?;
    Ok(archive.map(|archive| RecordedDependencies {
        // Native submodules have no URL provenance, but retain authenticated archive membership.
        repositories: archive
            .dependencies()
            .iter()
            .map(|dependency| dependency.repository.clone())
            .collect(),
        provenance: archive
            .dependencies()
            .iter()
            .flat_map(|dependency| dependency.provenance.clone())
            .collect(),
    }))
}

#[path = "dependency_diagnostics.rs"]
mod dependency_diagnostics_tests;

#[test]
fn export_recorded_dependencies_distinguishes_absence_from_corruption() {
    let root = super::make_run_dir(-789).unwrap();
    // 1. Missing review-archives directory -> Ok(None) (legacy absence)
    assert!(load_recorded_dependencies(
        &root.join("review-archives"),
        &crate::db::Db::for_test_at(root.join("review-archives")),
        42
    )
    .unwrap()
    .is_none());

    // 1b. Existing review-archives path that is a regular file -> Err(Integrity)
    let archives_dir = root.join("review-archives");
    std::fs::write(&archives_dir, b"not a directory").unwrap();
    assert!(matches!(
        load_recorded_dependencies(
            &root.join("review-archives"),
            &crate::db::Db::for_test_at(root.join("review-archives")),
            42
        ),
        Err(crate::review_archive::Error::Integrity)
    ));
    std::fs::remove_file(&archives_dir).unwrap();

    // 2. Existing store with no receipt for run -> Ok(None)
    let _store =
        crate::review_archive::ArchiveStore::open(&archives_dir, &crate::db::Db::for_test_at(&archives_dir)).unwrap();
    assert!(load_recorded_dependencies(
        &root.join("review-archives"),
        &crate::db::Db::for_test_at(root.join("review-archives")),
        42
    )
    .unwrap()
    .is_none());

    // 3. Existing store with receipt pointing to missing artifact -> Err(Integrity)
    let conn = crate::db::Db::for_test_at(&archives_dir).open().unwrap();
    conn.execute("INSERT INTO archive.receipts(owner, id) VALUES('review:42', '0000000000000000000000000000000000000000000000000000000000000000')", []).unwrap();
    drop(conn);
    assert!(matches!(
        load_recorded_dependencies(
            &root.join("review-archives"),
            &crate::db::Db::for_test_at(root.join("review-archives")),
            42
        ),
        Err(crate::review_archive::Error::Integrity)
    ));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Exports only one PR context through a read-only database connection; no Git or model calls"]
fn export_read_only_review_context_fixture() {
    use crate::db::pg::OptionalExtension;
    let repository = std::env::var("OCR_DIAGNOSTIC_REPOSITORY").expect("Repository required");
    let number: i64 = std::env::var("OCR_DIAGNOSTIC_PR")
        .expect("PR required")
        .parse()
        .unwrap();
    let db = crate::db::Db::diagnostic_from_env().open_read().unwrap();
    let conn = &db;
    let (pr_id, repo_id): (i64, i64) = conn.query_row("SELECT p.id,p.repo_id FROM pull_requests p JOIN repos r ON r.id=p.repo_id WHERE r.full_name=$1 AND p.number=$2", crate::pg_params![repository, number], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let size = conn
        .query_row("SELECT value FROM settings WHERE key='review_chunk_size'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|size| size.parse::<usize>().ok())
        .unwrap_or(5);
    let chunk_size = if size == 0 { 5 } else { size.clamp(2, 20) };
    let run = if let Ok(id) = std::env::var("OCR_DIAGNOSTIC_RUN") {
        db.review_run_by_id(id.parse().unwrap())
            .unwrap()
            .expect("Recorded run required")
    } else {
        db.latest_review_run(pr_id).unwrap().expect("Recorded run required")
    };
    assert_eq!(run.pr_id, pr_id, "Run belongs to another PR");
    let reprocess = std::env::var("OCR_DIAGNOSTIC_REPROCESS").as_deref() == Ok("true");
    assert!(
        diagnostic_run_is_eligible(
            &run.status,
            run.finished_at.is_some(),
            std::env::var("OCR_DIAGNOSTIC_RUN").is_ok(),
            reprocess
        ),
        "Only inspect a finished failed run or an explicitly selected finished reconstruction"
    );
    let scope = db.review_run_scope(run.id).unwrap().expect("Recorded scope required");
    let plan = db
        .context_plan_for_review_run(run.id)
        .unwrap()
        .expect("Recorded context required");
    let unresolved = db
        .latest_unresolved_review(pr_id)
        .unwrap()
        .filter(|(head, _)| reprocess || plan.prior_sha.as_deref() == Some(head.as_str()));
    let reprocess_source = if reprocess {
        db.reprocess_context_plan(pr_id, run.id).unwrap()
    } else {
        None
    };
    let reprocess_coverage = if reprocess {
        db.review_coverage_snapshot(pr_id, &run.head_sha).unwrap()
    } else {
        None
    };
    let prior_run: Option<i64> = conn.query_row("SELECT id FROM review_runs WHERE pr_id=$1 AND head_sha=$2 AND id<$3 AND review_body IS NOT NULL ORDER BY id DESC LIMIT 1", crate::pg_params![pr_id, plan.prior_sha, run.id], |row| row.get(0)).optional().unwrap();
    let prior_scope = prior_run.and_then(|id| db.review_run_scope(id).unwrap());
    let prior_coverage = if std::env::var("OCR_DIAGNOSTIC_REPLAN_BASE_SYNC").as_deref() == Ok("true") {
        plan.prior_sha
            .as_deref()
            .map(|head| db.review_coverage_snapshot(pr_id, head))
            .transpose()
            .unwrap()
            .flatten()
    } else {
        None
    };
    let (authorized_dependencies, recorded_dependencies) = match load_recorded_dependencies(
        &crate::review_archive::archive_dir(std::path::Path::new(
            &std::env::var("OCR_DATA_DIR").unwrap_or_else(|_| "data".into()),
        )),
        &db.handle(),
        run.id,
    )
    .expect("Review archive store and receipt must be valid if present")
    {
        Some(recorded) => (recorded.repositories, Some(recorded.provenance)),
        None => (std::collections::BTreeSet::new(), None),
    };
    let fixture = serde_json::json!({"reprocessSource":reprocess_source,"reprocessCoverage":reprocess_coverage,"authorizedDependencies":authorized_dependencies,"recordedDependencies":recorded_dependencies,"repository":repository,"number":number,"prId":pr_id,"repoId":repo_id,"runId":run.id,"model":run.model,"scope":scope,"priorScope":prior_scope,"priorCoverage":prior_coverage,"plan":plan,"chunkSize":chunk_size,"skill":db.active_skill_content(),"unresolved":unresolved});
    println!("READ_ONLY_REVIEW_FIXTURE {fixture}");
}

#[test]
#[ignore]
fn hydrate_pinned_review_dependencies_without_inference() {
    let packet: serde_json::Value = serde_json::from_str(&std::io::read_to_string(std::io::stdin()).unwrap()).unwrap();
    let fixture = &packet["fixture"];
    let repository = fixture["repository"].as_str().unwrap();
    let root = std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_REPOS").expect("Repository root required"));
    let db = crate::db::Db::diagnostic_from_env().open_read().unwrap();
    let run = db
        .review_run_by_id(fixture["runId"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(db.pr_review_coords(run.pr_id).unwrap().1, repository);
    assert_eq!(fixture["prId"].as_i64(), Some(run.pr_id));
    let explicit = std::env::var("OCR_DIAGNOSTIC_RUN")
        .ok()
        .and_then(|id| id.parse::<i64>().ok())
        == Some(run.id);
    let reprocess = std::env::var("OCR_DIAGNOSTIC_REPROCESS").as_deref() == Ok("true");
    assert!(
        diagnostic_run_is_eligible(&run.status, run.finished_at.is_some(), explicit, reprocess),
        "Only hydrate a finished failed run or an explicitly selected finished reconstruction"
    );
    let recorded = db.review_run_scope(run.id).unwrap().unwrap();
    let plan = db.context_plan_for_review_run(run.id).unwrap().unwrap();
    let scope = crate::review_archive::Scope {
        host: crate::github::gh_host(),
        repository: repository.into(),
        head_sha: recorded.head_sha,
        base_sha: recorded.base_sha,
        merge_base_sha: plan.merge_base_sha.unwrap(),
        prior_sha: plan.prior_sha.filter(|_| plan.incremental),
    };
    scope.validate().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let repo = root.join(repository);
    let archive = crate::review_archive::capture(&repo, scope, &cancel).unwrap();
    let authorized = std::collections::BTreeSet::new();
    let recorded_dependencies: Option<Vec<crate::review_archive::DependencyProvenance>> = fixture
        .get("recordedDependencies")
        .and_then(|val| val.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| serde_json::from_value(item.clone()).unwrap())
                .collect()
        });
    let archive = crate::review::dependencies::capture_with_recorded(
        archive,
        &repo,
        &plan.changed_files,
        packet["pr"]["body"].as_str().unwrap_or_default(),
        &authorized,
        true,
        recorded_dependencies.as_deref(),
        &cancel,
    )
    .unwrap();
    let provenance: Vec<crate::review_archive::DependencyProvenance> = archive
        .dependencies()
        .iter()
        .flat_map(|dependency| dependency.provenance.clone())
        .collect();
    // The current PR body may add or remove links since the recorded run.
    // Reusing recorded refs does not guarantee the original dependency snapshot.
    println!(
        "PINNED_DEPENDENCY_HYDRATION {}",
        serde_json::json!({
            "reconstruction": {
                "prBodySource": "current_github_pr",
                "referenceSource": "recorded_provenance_with_live_fallback",
                "historicalSnapshotGuaranteed": false,
            },
            "runId": run.id,
            "dependencies": archive.dependencies().iter().map(|dependency| serde_json::json!({
                "repository": dependency.repository, "revision": dependency.revision, "files": dependency.entries.len(),
            })).collect::<Vec<_>>(),
            "recordedDependencies": provenance,
            "authorizedDependencies": archive.dependencies().iter().map(|dependency| dependency.repository.clone()).collect::<std::collections::BTreeSet<_>>(),
            "modelCalls": 0,
            "databaseWrites": false,
        })
    );
}

#[test]
#[ignore = "Requires a read-only Git repository and exported review fixture on stdin; prepares inputs only"]
fn persisted_review_chunk_preparation_without_inference() {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin().take(8_388_609).read_to_string(&mut raw).unwrap();
    assert!(raw.len() <= 8_388_608, "Diagnostic fixture exceeds byte budget");
    let packet: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let fixture = &packet["fixture"];
    let pr_json = &packet["pr"];
    let repository = fixture["repository"].as_str().unwrap().to_string();
    let number = fixture["number"].as_i64().unwrap();
    let run_id = fixture["runId"].as_i64().unwrap();
    let scope: crate::review::scope::ReviewScope = serde_json::from_value(fixture["scope"].clone()).unwrap();
    let mut plan: crate::db::ReviewContextPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    let chunk_size = fixture["chunkSize"].as_u64().unwrap() as usize;
    let skill = fixture["skill"].as_str().unwrap().to_string();
    let unresolved: Option<(String, String)> = serde_json::from_value(fixture["unresolved"].clone()).unwrap();
    assert_eq!(
        pr_json["head"].as_str(),
        Some(scope.head_sha.as_str()),
        "PR head differs from the recorded run"
    );
    let root = super::make_run_dir(fixture["prId"].as_i64().unwrap()).unwrap();
    let context = root.join("context");
    std::fs::create_dir_all(&context).unwrap();
    let mut inputs = review_inputs("deep", 20, chunk_size);
    if let Some(model) = fixture["model"].as_str() {
        inputs.model = model.into();
    }
    inputs.run_id = run_id;
    inputs.pr_id = fixture["prId"].as_i64().unwrap();
    inputs.repo_id = fixture["repoId"].as_i64().unwrap();
    inputs.repo_path =
        std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_REPOS").expect("Read-only repository root required"))
            .join(&repository);
    inputs.full_name = repository;
    inputs.number = number;
    inputs.base_ref = scope.base_ref.clone();
    inputs.pr_title = pr_json["title"].as_str().unwrap().into();
    inputs.pr_body = pr_json["body"].as_str().unwrap_or_default().into();
    inputs.skill_content = skill;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    if std::env::var("OCR_DIAGNOSTIC_REPROCESS").as_deref() == Ok("true") {
        let (source_run, previous): (i64, crate::db::ReviewContextPlan) =
            serde_json::from_value(fixture["reprocessSource"].clone()).expect("Previous analysis scope required");
        let coverage: crate::review::scope::CoverageSnapshot =
            serde_json::from_value(fixture["reprocessCoverage"].clone()).expect("Verified coverage required");
        crate::review::scope::verify_coverage(&inputs.repo_path, &coverage, &cancel).unwrap();
        assert!(crate::review::scope::equivalent(
            &inputs.repo_path,
            &coverage.scope,
            &scope,
            &cancel
        ));
        let full = super::changed_files_local(
            &inputs.repo_path,
            &format!("{}...{}", scope.base_sha, scope.head_sha),
            &cancel,
        )
        .unwrap();
        let (assigned, selection) =
            crate::review::scope::reprocess_selection(&previous.changed_files, &full, source_run).unwrap();
        plan = super::build_snapshot_context_plan(
            &inputs,
            &assigned,
            &scope.head_sha,
            &scope.base_sha,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap()
        .0;
        plan.base_ref = Some(scope.base_ref.clone());
        plan.reviewed_base_sha = Some(scope.base_sha.clone());
        plan.merge_base_sha = Some(coverage.merge_base_sha);
        plan.head_sha = Some(scope.head_sha.clone());
        plan.base_sha = Some(scope.head_sha.clone());
        plan.incremental = false;
        plan.range = format!("{}...{}", scope.base_sha, scope.head_sha);
        plan.scope_selection = Some(selection.clone());
        plan.scope_reason = Some("Explicit re-run: fresh analysis of the previous scope".into());
        inputs.reprocess = true;
        assert!(!super::should_reuse_chunk_results(&inputs));
        println!(
            "OFFLINE_SCOPE_REPLAN {}",
            serde_json::json!({"runId":run_id,"sourceRun":source_run,"changedFiles":assigned,"selection":selection,"modelCalls":0})
        );
    }
    if std::env::var("OCR_DIAGNOSTIC_REPLAN_BASE_SYNC").as_deref() == Ok("true") {
        let prior: crate::review::scope::ReviewScope =
            serde_json::from_value(fixture["priorScope"].clone()).expect("Recorded prior review scope required");
        let coverage: crate::review::scope::CoverageSnapshot =
            serde_json::from_value(fixture["priorCoverage"].clone()).expect("Completed persisted coverage required");
        let mut selected = crate::review::scope::prepare_scope_reuse(
            &inputs.repo_path,
            Some(&prior),
            plan.prior_sha.as_deref(),
            &scope,
            Some(&coverage),
            false,
            &cancel,
        )
        .unwrap()
        .expect("Recorded history has no content-verified selection");
        crate::review::scope::exclude_policy_paths(&mut selected.changed_files, &mut selected.selection).unwrap();
        let recorded = plan;
        plan = super::build_snapshot_context_plan(
            &inputs,
            &selected.changed_files,
            &scope.head_sha,
            &scope.base_sha,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap()
        .0;
        plan.base_ref = Some(scope.base_ref.clone());
        plan.reviewed_base_sha = Some(scope.base_sha.clone());
        plan.merge_base_sha = recorded.merge_base_sha;
        plan.head_sha = Some(scope.head_sha.clone());
        plan.base_sha = Some(scope.head_sha.clone());
        plan.prior_sha = Some(prior.head_sha);
        plan.incremental = false;
        plan.range = format!("{}...{}", scope.base_sha, scope.head_sha);
        plan.scope_reason = Some("Base synchronized: review affected files against the current base; reuse content-verified coverage for unchanged files".into());
        plan.scope_selection = Some(selected.selection.clone());
        println!(
            "OFFLINE_SCOPE_REPLAN {}",
            serde_json::json!({"runId":run_id,"recordedFiles":recorded.changed_files.len(),"preparedFiles":plan.changed_files.len(),"changedFiles":plan.changed_files,"selection":selected.selection,"modelCalls":0})
        );
        if plan.changed_files.is_empty() {
            println!(
                "OFFLINE_CHUNK_PREPARATION {}",
                serde_json::json!({"runId":run_id,"chunks":[],"staticFinalBatches":0,"modelCalls":0,"selection":plan.scope_selection})
            );
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
    }
    if std::env::var("OCR_DIAGNOSTIC_REPLAN_INCREMENTAL").as_deref() == Ok("true") {
        let prior: crate::review::scope::ReviewScope =
            serde_json::from_value(fixture["priorScope"].clone()).expect("Recorded prior review scope required");
        assert_eq!(plan.prior_sha.as_deref(), Some(prior.head_sha.as_str()));
        assert!(
            crate::review::scope::can_review_incrementally(&inputs.repo_path, &prior, &scope, &cancel),
            "History does not prove a safe incremental scope"
        );
        let full_files = super::changed_files_local(
            &inputs.repo_path,
            &format!("{}...{}", scope.base_sha, scope.head_sha),
            &cancel,
        )
        .unwrap();
        let (paths, _) = super::incremental_review_changes(
            &inputs.repo_path,
            &prior.head_sha,
            &scope.head_sha,
            &full_files,
            &cancel,
        )
        .unwrap();
        let recorded = plan;
        plan = super::build_snapshot_context_plan(
            &inputs,
            &paths,
            &scope.head_sha,
            &scope.base_sha,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap()
        .0;
        plan.base_ref = Some(scope.base_ref.clone());
        plan.reviewed_base_sha = Some(scope.base_sha.clone());
        plan.merge_base_sha = recorded.merge_base_sha;
        plan.head_sha = Some(scope.head_sha.clone());
        plan.base_sha = Some(scope.head_sha.clone());
        plan.prior_sha = Some(prior.head_sha.clone());
        plan.incremental = true;
        plan.range = super::review_range(Some(&prior.head_sha), &scope.base_sha, &scope.head_sha);
        plan.scope_reason = Some("Incremental review with separate frozen publication anchors".into());
        println!(
            "OFFLINE_SCOPE_REPLAN {}",
            serde_json::json!({"runId":run_id,"recordedFiles":recorded.changed_files.len(),"preparedFiles":plan.changed_files.len(),"incremental":true,"modelCalls":0})
        );
    }
    if std::env::var("OCR_DIAGNOSTIC_REPLAN_PUBLICATION").as_deref() == Ok("true") && plan.incremental {
        let delta = super::diff_local_paths(&inputs.repo_path, &plan.range, &plan.changed_files, &cancel);
        let full_range = format!("{}...{}", scope.base_sha, scope.head_sha);
        let full = super::diff_local(&inputs.repo_path, &full_range, &cancel).unwrap();
        if !super::incremental_diff_is_publishable(&delta, &full) {
            let paths = super::changed_files_local(&inputs.repo_path, &full_range, &cancel).unwrap();
            let recorded = plan;
            plan = super::build_snapshot_context_plan(
                &inputs,
                &paths,
                &scope.head_sha,
                &scope.base_sha,
                &cancel,
                &mut |_, _| {},
            )
            .unwrap()
            .0;
            plan.base_ref = Some(scope.base_ref.clone());
            plan.reviewed_base_sha = Some(scope.base_sha.clone());
            plan.merge_base_sha = recorded.merge_base_sha;
            plan.head_sha = Some(scope.head_sha.clone());
            plan.base_sha = Some(scope.head_sha.clone());
            plan.prior_sha = recorded.prior_sha;
            plan.incremental = false;
            plan.range = full_range;
            plan.scope_reason = Some(
                "Full review required: incremental anchors cannot be published against the current PR diff".into(),
            );
            for name in [
                "head-context.sqlite3",
                "head-context.sqlite3-wal",
                "head-context.sqlite3-shm",
            ] {
                let path = root.join(name);
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                }
            }
            println!(
                "OFFLINE_SCOPE_REPLAN {}",
                serde_json::json!({"runId":run_id,"recordedFiles":recorded.changed_files.len(),"preparedFiles":plan.changed_files.len(),"incremental":false,"modelCalls":0})
            );
        }
    }
    let prior = if plan.incremental {
        plan.prior_sha.as_deref()
    } else {
        None
    };
    assert_eq!(plan.range, super::review_range(prior, &scope.base_sha, &scope.head_sha));
    let mut args = vec!["diff", plan.range.as_str(), "--"];
    args.extend(plan.changed_files.iter().map(String::as_str));
    let diff = String::from_utf8(super::git_capture(&inputs.repo_path, &args, &cancel, 60).unwrap()).unwrap();
    std::fs::write(context.join("PR_DIFF.patch"), &diff).unwrap();
    if plan.incremental {
        let full_range = format!("{}...{}", scope.base_sha, scope.head_sha);
        let full_diff = super::diff_local(&inputs.repo_path, &full_range, &cancel).unwrap();
        let full_files = super::changed_files_local(&inputs.repo_path, &full_range, &cancel).unwrap();
        std::fs::write(
            context.join("PUBLICATION_DIFF.patch"),
            super::integration::scoped_diff(&full_diff, &plan.changed_files, &full_files),
        )
        .unwrap();
    }
    let body = if inputs.pr_body.trim().is_empty() {
        "(no description provided)"
    } else {
        inputs.pr_body.trim()
    };
    std::fs::write(
        context.join("PR.md"),
        format!("# {}\n\n{}\n", inputs.pr_title.trim(), body),
    )
    .unwrap();
    if let Some((head, body)) = unresolved {
        std::fs::write(
            context.join("PENDING_REVIEW.md"),
            format!(
                "# Previously generated review awaiting decision\n\nHead: {head}\n\n{}\n",
                body.trim()
            ),
        )
        .unwrap();
    }
    super::materialize_files(
        &inputs.repo_path,
        &scope.base_sha,
        &scope.head_sha,
        &plan.changed_files,
        &plan,
        &context,
        &inputs.skill_content,
        true,
        &cancel,
    )
    .unwrap();
    for (area, reference, paths) in [
        ("changed", &scope.head_sha, plan.changed_files.clone()),
        (
            "related",
            &scope.head_sha,
            plan.related_files.iter().map(|file| file.path.clone()).collect(),
        ),
        ("conventions", &scope.base_sha, plan.convention_files.clone()),
    ] {
        for path in paths {
            let tree =
                super::git_capture(&inputs.repo_path, &["ls-tree", reference, "--", &path], &cancel, 15).unwrap();
            if tree.is_empty() && area == "changed" {
                continue;
            }
            let original = super::git_capture(
                &inputs.repo_path,
                &["show", &format!("{reference}:{path}")],
                &cancel,
                15,
            )
            .unwrap();
            assert!(
                std::fs::read(context.join(area).join(&path)).unwrap() == original,
                "Missing or altered {area}/{path}"
            );
        }
    }
    let merge_base =
        crate::review::scope::merge_base(&inputs.repo_path, &scope.base_sha, &scope.head_sha, &cancel).unwrap();
    assert_eq!(plan.merge_base_sha.as_deref(), Some(merge_base.as_str()));
    let archive_scope = crate::review_archive::Scope {
        host: crate::github::gh_host(),
        repository: inputs.full_name.clone(),
        head_sha: scope.head_sha.clone(),
        base_sha: scope.base_sha.clone(),
        merge_base_sha: merge_base.clone(),
        prior_sha: prior.map(str::to_string),
    };
    let archive = crate::review_archive::capture(&inputs.repo_path, archive_scope, &cancel).unwrap();
    let archive = if std::env::var("OCR_DIAGNOSTIC_PINNED_DEPENDENCIES").as_deref() == Ok("true") {
        let authorized = serde_json::from_value(fixture["authorizedDependencies"].clone())
            .expect("Authorized dependency repositories required");
        let recorded: Option<Vec<crate::review_archive::DependencyProvenance>> = fixture
            .get("recordedDependencies")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let captured = crate::review::dependencies::capture_with_recorded(
            archive,
            &inputs.repo_path,
            &plan.changed_files,
            &inputs.pr_body,
            &authorized,
            false,
            recorded.as_deref(),
            &cancel,
        )
        .unwrap();
        println!(
            "PINNED_DEPENDENCY_DIAGNOSTIC {}",
            serde_json::json!({"runId":run_id,"dependencies":captured.dependencies().iter().map(|dependency|serde_json::json!({"repository":dependency.repository,"revision":dependency.revision,"files":dependency.entries.len()})).collect::<Vec<_>>(),"modelCalls":0})
        );
        captured
    } else {
        archive
    };
    let manifest =
        crate::review_archive::transfer_manifest(&archive.descriptor(), &archive.manifest_bytes().unwrap()).unwrap();
    println!(
        "OFFLINE_ARCHIVE_DIAGNOSTIC {}",
        serde_json::json!({"runId":run_id,"archive":archive.descriptor(),"ungroupedManifestBytes":serde_json::to_vec(&manifest).unwrap().len(),"manifestLimitBytes":crate::review_archive::MAX_MANIFEST_BYTES,"modelCalls":0})
    );
    inputs.frozen = Some(std::sync::Arc::new(super::frozen::FrozenContext::new(archive)));
    let map = super::build_review_map(&inputs, &plan, &scope.head_sha, &plan.changed_files, &diff);
    super::write_review_map(&context, &map).unwrap();
    std::fs::write(
        context.join("CONTEXT.md"),
        super::render_context_md(&inputs, &plan, &scope.head_sha, &plan.changed_files),
    )
    .unwrap();
    super::frozen::prepare_stage(
        &inputs,
        &context,
        &map.reviewable_files,
        &diff,
        prior.unwrap_or(&merge_base),
    )
    .unwrap();
    let prompt_bytes =
        |chunk: &super::ReviewChunk| super::planned_chunk_prompt(&inputs, &root, chunk, &scope.head_sha, prior).len();
    let shared_files = ["PR.md", "PENDING_REVIEW.md", "SCOPE_CONTEXT.md", "skill/SKILL.md"]
        .into_iter()
        .map(|path| (path.to_string(), super::file_bytes(&context.join(path))))
        .chain(plan.convention_files.iter().map(|path| {
            (
                format!("conventions/{path}"),
                super::file_bytes(&context.join("conventions").join(path)),
            )
        }))
        .collect::<std::collections::BTreeMap<_, _>>();
    let patches = super::chunk_diff_bytes(&map, &diff);
    for path in &map.reviewable_files {
        let chunk = super::ReviewChunk {
            id: 1,
            files: vec![path.clone()],
            related_files: vec![],
            global_context_files: vec![],
            omitted_convention_files: vec![],
            risk_triggers: vec![],
            cross_chunk_group_ids: vec![],
        };
        let scoped = super::chunk_review_map(&map, &chunk);
        let budget = super::chunk_context_budget(&map, &context, &chunk, &patches, prompt_bytes(&chunk)).unwrap();
        if budget.validate().is_err() {
            println!(
                "OFFLINE_CONTEXT_DIAGNOSTIC {}",
                serde_json::json!({"path":path,"sharedFiles":shared_files,"mapJsonBytes":serde_json::to_vec_pretty(&scoped).unwrap().len(),"mapMarkdownBytes":super::render_review_map_md(&scoped).len(),"descriptionBytes":scoped.description.len(),"sourceBytes":budget.files,"diffBytes":budget.diff,"promptBytes":budget.prompt})
            );
        }
    }
    let chunks = super::build_graph_chunks(&map, &context, chunk_size, &prompt_bytes).unwrap();
    let planned_chunks = chunks.iter().map(|chunk| {
        let budget = super::chunk_context_budget(&map, &context, chunk, &patches, prompt_bytes(chunk)).unwrap();
        serde_json::json!({"id":chunk.id,"files":chunk.files,"relatedFiles":chunk.related_files,"globalFiles":chunk.global_context_files,"sourceBytes":budget.files,"diffBytes":budget.diff,"metadataBytes":budget.metadata,"sharedBytes":budget.shared,"promptBytes":budget.prompt})
    }).collect::<Vec<_>>();
    println!(
        "OFFLINE_CHUNK_PLAN {}",
        serde_json::json!({"runId":run_id,"reviewableFiles":map.reviewable_files.len(),"chunkSize":chunk_size,"chunkCount":chunks.len(),"chunkCountLimit":serde_json::Value::Null,"sharedFiles":shared_files,"chunks":planned_chunks})
    );
    let patches = super::chunk_diff_bytes(&map, &diff);
    let mut measurements = Vec::new();
    for chunk in &chunks {
        let budget = super::chunk_context_budget(&map, &context, chunk, &patches, prompt_bytes(chunk)).unwrap();
        super::prepare_review_chunk(
            &inputs,
            &cancel,
            &root,
            &context,
            &map,
            chunk,
            &scope.head_sha,
            prior,
            &scope.base_sha,
        )
        .unwrap();
        let directory = root.join(format!("chunks/{:03}", chunk.id));
        let chunk_context = directory.join("context");
        for (source_area, target_area, paths) in [
            ("changed", "changed", &chunk.files),
            ("related", "related", &chunk.related_files),
            ("changed", "global", &chunk.global_context_files),
        ] {
            for path in paths {
                let source = context.join(source_area).join(path);
                if source.is_file() {
                    assert!(
                        std::fs::read(chunk_context.join(target_area).join(path)).unwrap()
                            == std::fs::read(source).unwrap(),
                        "Altered chunk evidence: {path}"
                    );
                }
            }
        }
        assert!(
            std::fs::read(chunk_context.join("PR.md")).unwrap() == std::fs::read(context.join("PR.md")).unwrap(),
            "Altered PR description"
        );
        assert!(
            std::fs::read(chunk_context.join("skill/SKILL.md")).unwrap()
                == std::fs::read(context.join("skill/SKILL.md")).unwrap(),
            "Altered chunk skill"
        );
        for path in &plan.convention_files {
            if chunk.omitted_convention_files.contains(path) {
                assert!(!chunk_context.join("conventions").join(path).exists());
            } else {
                assert!(
                    std::fs::read(chunk_context.join("conventions").join(path)).unwrap()
                        == std::fs::read(context.join("conventions").join(path)).unwrap(),
                    "Altered chunk convention: {path}"
                );
            }
        }
        let mut args = vec!["diff", plan.range.as_str(), "--"];
        args.extend(chunk.files.iter().map(String::as_str));
        assert!(
            std::fs::read(chunk_context.join("PR_DIFF.patch")).unwrap()
                == super::git_capture(&inputs.repo_path, &args, &cancel, 60).unwrap(),
            "Altered chunk diff"
        );
        let actual = super::dir_bytes(&chunk_context) + super::file_bytes(&directory.join("prompt.md"));
        assert!(
            actual as usize <= budget.total(),
            "Chunk {} actual {actual} exceeds planned {}",
            chunk.id,
            budget.total()
        );
        super::validate_review_stage_inputs(&chunk_context, None, &directory.join("prompt.md")).unwrap();
        assert_eq!(super::file_bytes(&directory.join("transcript.json")), 0);
        measurements.push(serde_json::json!({"chunk":chunk.id,"files":chunk.files.len(),"plannedBytes":budget.total(),"actualBytes":actual,"promptBytes":budget.prompt,"overSelectionTarget":actual as usize > crate::review::policy::TARGET_CONTEXT_BYTES,"omittedConventions":chunk.omitted_convention_files}));
    }
    let mut covered = chunks.iter().flat_map(|chunk| chunk.files.clone()).collect::<Vec<_>>();
    covered.sort();
    let mut expected = map.reviewable_files.clone();
    expected.sort();
    assert_eq!(covered, expected);
    let full_diff = String::from_utf8(
        super::git_capture(
            &inputs.repo_path,
            &["diff", &format!("{}...{}", scope.base_sha, scope.head_sha)],
            &cancel,
            60,
        )
        .unwrap(),
    )
    .unwrap();
    let final_stages = super::integration::prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("integration-static"),
        &super::integration::Payload::empty(&scope.head_sha),
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &cancel,
    )
    .unwrap();
    let static_final_batches = final_stages.len();
    let mut stages = chunks
        .iter()
        .map(|chunk| {
            let directory = root.join(format!("chunks/{:03}", chunk.id));
            (
                directory.clone(),
                directory.join("context"),
                directory.join("prompt.md"),
            )
        })
        .collect::<Vec<_>>();
    stages.extend(
        final_stages
            .into_iter()
            .map(|stage| (stage.directory, stage.context, stage.prompt)),
    );
    let mut initial_bytes = 0usize;
    let mut largest_stage_bytes = 0usize;
    super::frozen::for_each_prepared_request(
        &inputs,
        &root,
        &stages,
        "offline",
        1,
        "claude",
        &cancel,
        |_, request| {
            if let crate::sandbox::runner::Request::Run { files, .. } = request {
                let bytes = files
                    .iter()
                    .map(|(path, content)| path.len() + content.len())
                    .sum::<usize>();
                initial_bytes += bytes;
                largest_stage_bytes = largest_stage_bytes.max(bytes);
            }
            Ok(())
        },
    )
    .unwrap();
    let archive = &inputs.frozen.as_ref().unwrap().archive;
    let materialized = root.join("evidence");
    archive.materialize_new(&materialized, &cancel).unwrap();
    for entry in archive.entries().iter().filter(|entry| entry.exclusion.is_none()) {
        let source = &entry.source;
        assert!(
            std::fs::read(materialized.join("trees").join(&source.revision).join(&source.path)).unwrap()
                == archive.resolve(&source.revision, &source.path).unwrap().as_bytes(),
            "Materialized archive differs at {}",
            source.path
        );
    }
    let mut runtime_preflights = 0;
    if let Ok(sockets) = std::env::var("OCR_DIAGNOSTIC_RUNNER_SOCKETS") {
        assert!(
            fixture["model"].as_str().is_some(),
            "Recorded model required for runtime preflight"
        );
        let sockets = sockets.split(',').map(std::path::PathBuf::from).collect::<Vec<_>>();
        assert!(!sockets.is_empty() && sockets.len() <= 3 && sockets.iter().all(|path| path.is_absolute()));
        for (runner, socket) in sockets.iter().enumerate() {
            let (models, version) = crate::sandbox::runner::catalog_at(socket).unwrap();
            assert!(
                models.contains(&inputs.model),
                "Recorded model unavailable in runner {}",
                runner + 1
            );
            super::frozen::for_each_prepared_request(&inputs, &root, &stages, &version, 1, "claude", &cancel, |stage, request| {
                let result = crate::sandbox::runner::preflight_at(socket, archive, &request, &cancel);
                println!("RUNTIME_PREFLIGHT {}", serde_json::json!({"runner":runner + 1,"stage":stage + 1,"stages":stages.len(),"ready":result.is_ok(),"error":result.as_ref().err(),"modelCalls":0}));
                result?;
                runtime_preflights += 1;
                Ok(())
            }).unwrap();
        }
    }
    if let Ok(export) = std::env::var("OCR_DIAGNOSTIC_EXPORT_STAGE") {
        let export = std::path::PathBuf::from(export);
        assert!(export.is_dir(), "Private diagnostic directory required");
        archive.write_new(&export.join("snapshot.archive")).unwrap();
        if std::env::var("OCR_DIAGNOSTIC_EXPORT_ALL").as_deref() == Ok("true") {
            let prepared = export.join("prepared");
            super::copy_tree(&root, &prepared);
            let relative_stages = stages
                .iter()
                .map(|(directory, context, prompt)| {
                    (
                        directory.strip_prefix(&root).unwrap(),
                        context.strip_prefix(&root).unwrap(),
                        prompt.strip_prefix(&root).unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            for (_, _, prompt) in &relative_stages {
                let original = std::fs::read_to_string(root.join(prompt)).unwrap();
                std::fs::write(
                    prepared.join(prompt),
                    original.replace(root.to_str().unwrap(), prepared.to_str().unwrap()),
                )
                .unwrap();
            }
            for chunk in &chunks {
                let directory = prepared.join(format!("chunks/{:03}", chunk.id));
                let request = crate::sandbox::runner::stage_request(
                    &inputs.model,
                    "offline",
                    1,
                    "claude",
                    &prepared,
                    &directory,
                    &directory.join("context"),
                    &directory.join("prompt.md"),
                )
                .unwrap();
                let packet = serde_json::json!({"runId":run_id,"chunk":chunk.id,"files":chunk.files,"descriptor":archive.descriptor(),"engineVersion":crate::sandbox::engine_version(),"request":request});
                std::fs::write(directory.join("stage.json"), serde_json::to_vec(&packet).unwrap()).unwrap();
            }
            let mut prepared_fixture = fixture.clone();
            prepared_fixture["plan"] = serde_json::to_value(&plan).unwrap();
            let packet = serde_json::json!({"runId":run_id,"fixture":prepared_fixture,"recordedPlan":fixture["plan"],"pr":pr_json,"map":map,"chunks":chunks,"stages":relative_stages,"fullDiff":full_diff,"descriptor":archive.descriptor(),"engineVersion":crate::sandbox::engine_version()});
            std::fs::write(export.join("coordinator.json"), serde_json::to_vec(&packet).unwrap()).unwrap();
            println!(
                "EXPORTED_DIAGNOSTIC_STAGE {}",
                serde_json::json!({"runId":run_id,"chunks":chunks.len(),"modelCalls":0})
            );
        } else {
            let selected: usize = std::env::var("OCR_DIAGNOSTIC_CHUNK")
                .expect("Explicit chunk required")
                .parse()
                .unwrap();
            let chunk = chunks
                .iter()
                .find(|chunk| chunk.id == selected)
                .expect("Unknown diagnostic chunk");
            let directory = root.join(format!("chunks/{selected:03}"));
            let request = crate::sandbox::runner::stage_request(
                &inputs.model,
                "offline",
                1,
                "claude",
                &root,
                &directory,
                &directory.join("context"),
                &directory.join("prompt.md"),
            )
            .unwrap();
            let packet = serde_json::json!({"runId":run_id,"chunk":selected,"files":chunk.files,"descriptor":archive.descriptor(),"engineVersion":crate::sandbox::engine_version(),"request":request});
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(export.join("stage.json"))
                .unwrap();
            std::io::Write::write_all(&mut file, &serde_json::to_vec(&packet).unwrap()).unwrap();
            println!(
                "EXPORTED_DIAGNOSTIC_STAGE {}",
                serde_json::json!({"runId":run_id,"chunk":selected,"files":chunk.files,"modelCalls":0})
            );
        }
    }
    println!(
        "OFFLINE_CHUNK_PREPARATION {}",
        serde_json::json!({"runtimePreflights":runtime_preflights,"staticFinalBatches":static_final_batches,"runId":run_id,"head":scope.head_sha,"base":scope.base_sha,"changedFiles":plan.changed_files.len(),"reviewableFiles":map.reviewable_files.len(),"omittedFiles":map.omitted_noise_files,"relatedFiles":plan.related_files.len(),"archive":inputs.frozen.as_ref().unwrap().archive.descriptor(),"preparedStageBytes":initial_bytes,"largestStageBytes":largest_stage_bytes,"materializedArchiveBytes":super::dir_bytes(&materialized),"preparedStages":stages.len(),"sharedBytes":super::chunk_context_budget(&map,&context,&chunks[0],&patches,prompt_bytes(&chunks[0])).unwrap().shared,"chunks":measurements,"modelCalls":0})
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Revalidates retained final reports and finishes the UI audit offline; no model calls"]
fn verify_retained_final_and_audit() {
    use serde_json::json;
    let source = std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_EXPORT_STAGE").unwrap());
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("diagnostic-result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "complete", "Only verify a completed real execution");
    let packet: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("coordinator.json")).unwrap()).unwrap();
    let root = source.join("prepared");
    let descriptor = serde_json::from_value(packet["descriptor"].clone()).unwrap();
    let archive = crate::review_archive::Archive::read(&source.join("snapshot.archive"), &descriptor).unwrap();
    let map: super::ReviewMap = serde_json::from_value(packet["map"].clone()).unwrap();
    let mut inputs = review_inputs("deep", 20, 5);
    let db_path = source.join("diagnostic.sqlite");
    inputs.db_handle = Some(crate::db::Db::for_test_at(db_path.clone()));
    inputs.archive_dir = source.join("review-archives");
    inputs.frozen = Some(std::sync::Arc::new(super::frozen::FrozenContext::new(archive)));
    let mut payload = super::integration::Payload::empty(&map.head_sha);
    payload.sources = serde_json::from_slice(&std::fs::read(root.join("merge/chunk_findings.json")).unwrap()).unwrap();
    let mut directories = std::fs::read_dir(root.join("merge/batches"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    directories.sort();
    let mut outputs = Vec::new();
    for directory in directories {
        let context = directory.join("context");
        let stage: crate::review_archive::Stage =
            serde_json::from_slice(&std::fs::read(context.join("STAGE.json")).unwrap()).unwrap();
        let candidates: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("chunk_findings.json")).unwrap()).unwrap();
        let prepared = super::integration::Prepared {
            prompt: directory.join("prompt.md"),
            diff: std::fs::read_to_string(context.join("PR_DIFF.patch")).unwrap(),
            files: stage.files,
            candidate_ids: candidates["comments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["candidateId"].as_str().unwrap().to_string())
                .collect(),
            directory,
            context,
        };
        outputs.push(super::integration::validate_output(&inputs, &prepared, &payload, &map.head_sha).unwrap());
    }
    let (body, comments, evidence_warnings) = super::integration::combine(
        &inputs,
        &payload,
        outputs,
        &map.reviewable_files,
        &map.head_sha,
        packet["fullDiff"].as_str().unwrap(),
        &root.join("merge"),
    )
    .unwrap();
    assert_eq!(comments, std::fs::read_to_string(root.join("comments.json")).unwrap());
    assert_eq!(body, std::fs::read_to_string(root.join("summary.md")).unwrap());
    let audit = super::diagnostics::finish_audit(
        &inputs,
        &root,
        &body,
        &comments,
        packet["fullDiff"].as_str().unwrap(),
        &evidence_warnings,
    )
    .unwrap();
    let summary = json!({"runId":packet["runId"],"head":map.head_sha,"reviewedFiles":map.reviewable_files.len(),"sourceCandidates":payload.sources["comments"].as_array().unwrap().len(),"finalComments":serde_json::from_str::<serde_json::Value>(&comments).unwrap()["comments"].as_array().unwrap().len(),"auditComplete":audit.complete,"auditCandidates":audit.candidates.len(),"evidenceWarnings":audit.evidence_warnings,"modelCalls":0,"published":false});
    std::fs::write(
        source.join("audit-verification.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("VERIFIED_REAL_FINAL {summary}");
}

fn repair_diagnostic_final(
    inputs: &super::ReviewInputs,
    root: &std::path::Path,
    map: &super::ReviewMap,
    plan: &crate::db::ReviewContextPlan,
    full_diff: &str,
    results: &[super::ChunkResult],
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
) -> Result<super::ReviewOutput, String> {
    use std::fs;
    let previous_root =
        std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_REUSE_REVIEW").map_err(|_| "Retained review required")?)
            .join("prepared");
    let batches = fs::read_dir(previous_root.join("merge/batches"))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if batches.len() != 1 {
        return Err("One-call repair requires exactly one retained Final batch".into());
    }
    let previous = batches[0].path();
    let request = crate::sandbox::runner::stage_request(
        &inputs.model,
        "offline",
        1,
        "claude",
        &previous_root,
        &previous,
        &previous.join("context"),
        &previous.join("prompt.md"),
    )?;
    let crate::sandbox::runner::Request::Run { files, prompt, .. } = request else {
        return Err("Retained Final is not a model stage".into());
    };
    inputs
        .frozen
        .as_ref()
        .unwrap()
        .archive
        .validate_stage_files(&files)
        .map_err(|error| error.to_string())?;
    let directory = root.join("merge/batches/batch-001");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    for (name, content) in files {
        if matches!(
            name.as_str(),
            "integration/rejected-report.json" | "integration/rejected-decisions.json"
        ) {
            continue;
        }
        let name = name.strip_prefix("integration/").unwrap_or(&name);
        let destination = directory.join(name);
        fs::create_dir_all(destination.parent().unwrap()).map_err(|error| error.to_string())?;
        fs::write(destination, content).map_err(|error| error.to_string())?;
    }
    fs::write(directory.join("prompt.md"), prompt).map_err(|error| error.to_string())?;
    fs::copy(previous.join("rejected-report.json"), directory.join("report.json"))
        .map_err(|error| error.to_string())?;
    fs::copy(
        previous.join("rejected-decisions.json"),
        directory.join("finding_decisions.json"),
    )
    .map_err(|error| error.to_string())?;
    let context = directory.join("context");
    let stage: crate::review_archive::Stage =
        serde_json::from_slice(&fs::read(context.join("STAGE.json")).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let candidates: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("chunk_findings.json")).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let prepared = super::integration::Prepared {
        prompt: directory.join("prompt.md"),
        diff: fs::read_to_string(context.join("PR_DIFF.patch")).map_err(|error| error.to_string())?,
        files: stage.files,
        candidate_ids: candidates["comments"]
            .as_array()
            .ok_or("Missing Final candidates")?
            .iter()
            .map(|candidate| candidate["candidateId"].as_str().unwrap().to_string())
            .collect(),
        directory,
        context,
    };
    let sources: serde_json::Value = serde_json::from_slice(
        &fs::read(previous_root.join("merge/chunk_findings.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let payload = super::integration::Payload::from_results(results, sources)?;
    inputs.open_db()?.store_finding_audit(
        inputs.run_id,
        &crate::review::findings::seed_audit(&payload.sources, &map.head_sha, full_diff),
    )?;
    fs::write(
        root.join("merge/chunk_findings.json"),
        serde_json::to_vec(&payload.sources).unwrap(),
    )
    .map_err(|error| error.to_string())?;
    let transcript = prepared.directory.join("transcript.json");
    let started = super::now_millis();
    let repaired = super::output_repair::validate(
        inputs,
        cancel,
        progress,
        &super::SpawnPaths {
            config: &root.join("unused-config.json"),
            prompt: &prepared.prompt,
            transcript: &transcript,
            stdout: &prepared.directory.join("claude.stdout"),
            stderr: &prepared.directory.join("claude.stderr"),
            progress_phase: "final",
            sharded_progress: true,
        },
        &prepared.context,
        || super::integration::validate_output(inputs, &prepared, &payload, &map.head_sha),
    );
    let usage = super::parse_transcript_usage(&transcript);
    super::record_stage(
        inputs,
        "final",
        None,
        if repaired.is_ok() { "done" } else { "failed" },
        None,
        false,
        &usage,
        super::StageMetrics {
            prompt_bytes: super::file_bytes(&prepared.prompt),
            diff_bytes: prepared.diff.len() as i64,
            context_bytes: super::dir_bytes(&prepared.context),
            file_count: prepared.files.len() as i64,
            related_file_count: 0,
        },
        started,
        super::now_millis(),
    );
    let (body, comments, evidence_warnings) = super::integration::combine(
        inputs,
        &payload,
        vec![repaired?],
        &map.reviewable_files,
        &map.head_sha,
        full_diff,
        &root.join("merge"),
    )?;
    fs::write(root.join("summary.md"), &body).map_err(|error| error.to_string())?;
    fs::write(root.join("comments.json"), &comments).map_err(|error| error.to_string())?;
    Ok(super::ReviewOutput {
        evidence_warnings,
        risk: super::score_review_risk(plan, full_diff, Some(&comments)),
        body,
        comments_json: Some(comments),
        usage,
        no_new_changes: false,
    })
}

fn record_diagnostic_attempt(
    attempts: &mut Vec<std::path::PathBuf>,
    stage: &std::path::Path,
    limit: usize,
) -> Result<(), String> {
    if attempts.len() >= limit || attempts.iter().any(|path| path == stage) {
        return Err("Diagnostic authorization forbids additional inference or repeating a stage".into());
    }
    attempts.push(stage.into());
    Ok(())
}

#[test]
fn diagnostic_budget_allows_distinct_final_batches_without_retries_or_overruns() {
    let mut attempts = Vec::new();
    for stage in [
        "chunks/001",
        "chunks/002",
        "merge/batches/batch-001",
        "merge/batches/batch-002",
    ] {
        record_diagnostic_attempt(&mut attempts, std::path::Path::new(stage), 4).unwrap();
        assert!(record_diagnostic_attempt(&mut attempts, std::path::Path::new(stage), 4).is_err());
    }
    assert_eq!(attempts.len(), 4);
    assert!(record_diagnostic_attempt(&mut attempts, std::path::Path::new("merge/batches/batch-003"), 4).is_err());
    assert_eq!(attempts.len(), 4);
}

pub(super) fn guard_diagnostic_execution(stage: &std::path::Path) -> Result<(), String> {
    let Ok(limit) = std::env::var("OCR_DIAGNOSTIC_MAX_EXECUTIONS") else {
        return Ok(());
    };
    let limit = limit
        .parse::<usize>()
        .map_err(|_| "Invalid diagnostic execution limit")?;
    static ATTEMPTS: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut attempts = ATTEMPTS.lock().map_err(|_| "Diagnostic execution guard unavailable")?;
    record_diagnostic_attempt(&mut attempts, stage, limit)?;
    let root = std::path::PathBuf::from(
        std::env::var("OCR_DIAGNOSTIC_EXPORT_STAGE").map_err(|_| "Diagnostic output unavailable")?,
    );
    std::fs::write(
        root.join("paid-attempts.json"),
        serde_json::to_vec_pretty(&*attempts).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

#[test]
#[ignore = "Paid isolated full review; requires explicit one-run authorization and a dedicated runner"]
fn validate_complete_real_review() {
    use serde_json::json;
    use std::io::Write;
    let source = std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_EXPORT_STAGE").unwrap());
    let packet: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("coordinator.json")).unwrap()).unwrap();
    assert_eq!(
        std::env::var("OCR_DIAGNOSTIC_FULL_REVIEW_AUTHORIZED").unwrap(),
        packet["runId"].as_i64().unwrap().to_string()
    );
    assert_eq!(
        packet["engineVersion"].as_str(),
        Some(crate::sandbox::engine_version().as_str())
    );
    let root = source.join("prepared");
    let context = root.join("context");
    let repair_final = std::env::var("OCR_DIAGNOSTIC_REPAIR_FINAL_ONLY").as_deref() == Ok("true");
    let final_only = repair_final || std::env::var("OCR_DIAGNOSTIC_FINAL_ONLY").as_deref() == Ok("true");
    if repair_final {
        assert_eq!(std::env::var("OCR_DIAGNOSTIC_MAX_EXECUTIONS").as_deref(), Ok("1"));
    }
    let fixture = &packet["fixture"];
    let plan: crate::db::ReviewContextPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    let map: super::ReviewMap = serde_json::from_value(packet["map"].clone()).unwrap();
    let chunks: Vec<super::ReviewChunk> = serde_json::from_value(packet["chunks"].clone()).unwrap();
    let descriptor = serde_json::from_value(packet["descriptor"].clone()).unwrap();
    let archive = crate::review_archive::Archive::read(&source.join("snapshot.archive"), &descriptor).unwrap();
    let sockets = std::env::var("OCR_DIAGNOSTIC_RUNNER_SOCKETS")
        .or_else(|_| std::env::var("OCR_DIAGNOSTIC_RUNNER_SOCKET"))
        .unwrap();
    let sockets = sockets.split(',').map(std::path::PathBuf::from).collect::<Vec<_>>();
    assert!(!sockets.is_empty() && sockets.len() <= 3);
    let mut targets = Vec::new();
    let mut available = None;
    for socket in sockets {
        let (models, version) = crate::sandbox::runner::catalog_at(&socket).unwrap();
        assert!(models.iter().any(|model| model == fixture["model"].as_str().unwrap()));
        available = Some(models);
        targets.push((socket, version));
    }
    let models = available.unwrap();
    assert!(targets.iter().all(|(_, version)| version == &targets[0].1));
    let mut inputs = review_inputs("deep", 20, fixture["chunkSize"].as_u64().unwrap() as usize);
    inputs.model = fixture["model"].as_str().unwrap().into();
    assert!(models.contains(&inputs.model));
    inputs.full_name = fixture["repository"].as_str().unwrap().into();
    inputs.number = fixture["number"].as_i64().unwrap();
    inputs.pr_title = packet["pr"]["title"].as_str().unwrap().into();
    inputs.pr_body = packet["pr"]["body"].as_str().unwrap_or_default().into();
    inputs.skill_content = fixture["skill"].as_str().unwrap().into();
    inputs.fixed_head_sha = Some(map.head_sha.clone());
    inputs.review_parallel_chunks = targets.len();
    let db_path = source.join("diagnostic.sqlite");
    inputs.db_handle = Some(crate::db::Db::for_test_at(db_path.clone()));
    inputs.archive_dir = source.join("review-archives");
    assert!(!db_path.exists(), "Diagnostic ledger already exists");
    drop(inputs.open_db().unwrap());
    let ledger = crate::db::Db::for_test_at(&db_path).open().unwrap();
    ledger
        .execute(
            "INSERT INTO repos(id,owner,name,full_name,created_at,updated_at) VALUES(1,'diagnostic','repo',$1,0,0)",
            [&inputs.full_name],
        )
        .unwrap();
    ledger.execute("INSERT INTO pull_requests(id,repo_id,number,title,author,head_sha,last_synced_at,created_at,updated_at) VALUES(1,1,$1,$2,'diagnostic',$3,0,0,0)", crate::pg_params![inputs.number,inputs.pr_title,map.head_sha]).unwrap();
    ledger
        .execute(
            "INSERT INTO review_runs(id,pr_id,head_sha,status,created_at,updated_at) VALUES(1,1,$1,'running',0,0)",
            [&map.head_sha],
        )
        .unwrap();
    inputs.frozen = Some(std::sync::Arc::new(super::frozen::FrozenContext::diagnostic(
        archive,
        root.clone(),
        targets,
    )));
    let relative: Vec<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> =
        serde_json::from_value(packet["stages"].clone()).unwrap();
    let stages = relative
        .into_iter()
        .map(|(stage, context, prompt)| (root.join(stage), root.join(context), root.join(prompt)))
        .collect::<Vec<_>>();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    println!(
        "REAL_REVIEW_PROGRESS {}",
        json!({"phase":"preflight","stages":stages.len(),"modelCalls":0})
    );
    super::frozen::preflight_additional(&inputs, &stages, &cancel).unwrap();
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(source.join("full-execution-once.json"))
        .unwrap();
    writeln!(
        marker,
        "{}",
        json!({"runId":packet["runId"],"model":inputs.model,"chunks":chunks.len()})
    )
    .unwrap();
    let mut events = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(source.join("progress.jsonl"))
        .unwrap();
    let mut last = String::new();
    let mut last_phase = String::new();
    let mut progress = |phase: &str, tail: &str| {
        if phase == last_phase {
            return;
        }
        last_phase = phase.into();
        writeln!(events,"{}",json!({"at":super::now_millis(),"phase":serde_json::from_str::<serde_json::Value>(phase).unwrap_or_default(),"tail":tail})).unwrap();
        events.flush().unwrap();
        let headline = tail.lines().next().unwrap_or_default();
        if headline != last {
            println!("REAL_REVIEW_PROGRESS {}", json!({"detail":headline}));
            last = headline.into();
        }
    };
    let result = (|| -> Result<super::ReviewOutput, String> {
        let mut results = Vec::new();
        if let Ok(reuse) = std::env::var("OCR_DIAGNOSTIC_REUSE_REVIEW") {
            let reuse = std::path::PathBuf::from(reuse).join("prepared/chunks");
            for chunk in &chunks {
                let previous = reuse.join(format!("{:03}", chunk.id));
                if !previous.join("report.json").is_file() {
                    break;
                }
                let directory = root.join(format!("chunks/{:03}", chunk.id));
                let previous_packet: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(previous.join("stage.json")).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let request = crate::sandbox::runner::stage_request(
                    &inputs.model,
                    "offline",
                    1,
                    "claude",
                    &root,
                    &directory,
                    &directory.join("context"),
                    &directory.join("prompt.md"),
                )?;
                if serde_json::to_value(request).unwrap() != previous_packet["request"] {
                    return Err(format!(
                        "Retained chunk {} has different inputs; refusing to reuse it",
                        chunk.id
                    ));
                }
                std::fs::copy(previous.join("report.json"), directory.join("report.json"))
                    .map_err(|error| error.to_string())?;
                let diff = std::fs::read_to_string(directory.join("context/PR_DIFF.patch"))
                    .map_err(|error| error.to_string())?;
                let started = super::now_millis();
                let validate = || {
                    super::frozen::materialize(
                        &inputs,
                        &directory.join("report.json"),
                        &directory.join("context"),
                        &chunk.files,
                        &map.head_sha,
                        &diff,
                        &directory.join("summary.md"),
                        &directory.join("comments.json"),
                    )
                };
                let result = if final_only {
                    validate()
                } else {
                    super::output_repair::validate(
                        &inputs,
                        &cancel,
                        &mut progress,
                        &super::SpawnPaths {
                            config: &root.join("unused-config.json"),
                            prompt: &directory.join("prompt.md"),
                            transcript: &directory.join("transcript.json"),
                            stdout: &directory.join("claude.stdout"),
                            stderr: &directory.join("claude.stderr"),
                            progress_phase: "chunks",
                            sharded_progress: true,
                        },
                        &directory.join("context"),
                        validate,
                    )
                };
                let repaired = directory.join("rejected-report.json").exists();
                let usage = super::parse_transcript_usage(&directory.join("transcript.json"));
                let status = match &result {
                    Err(error) if error == "cancelled" => "cancelled",
                    Err(_) => "failed",
                    Ok(_) if repaired => "done",
                    Ok(_) => "cached",
                };
                super::record_stage(
                    &inputs,
                    "chunk",
                    Some(chunk.id as i64),
                    status,
                    None,
                    false,
                    &usage,
                    super::StageMetrics {
                        prompt_bytes: super::file_bytes(&directory.join("prompt.md")),
                        diff_bytes: diff.len() as i64,
                        context_bytes: super::dir_bytes(&directory.join("context")),
                        file_count: chunk.files.len() as i64,
                        related_file_count: chunk.related_files.len() as i64,
                    },
                    started,
                    super::now_millis(),
                );
                let restored = super::diagnostics::restore_chunk(chunk, &directory, result, !repaired, usage.clone())?;
                println!(
                    "REAL_REVIEW_PROGRESS {}",
                    json!({"chunk":chunk.id,"cached":!repaired,"repaired":repaired,"newTokens":usage.input_tokens + usage.output_tokens})
                );
                results.push(restored);
            }
        } else if let Ok(reuse) = std::env::var("OCR_DIAGNOSTIC_REUSE_CHUNK") {
            let reuse = std::path::PathBuf::from(reuse);
            let previous: serde_json::Value =
                serde_json::from_slice(&std::fs::read(reuse.join("stage.json")).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
            let first = root.join("chunks/001");
            let request = crate::sandbox::runner::stage_request(
                &inputs.model,
                "offline",
                1,
                "claude",
                &root,
                &first,
                &first.join("context"),
                &first.join("prompt.md"),
            )?;
            if serde_json::to_value(request).unwrap() == previous["request"] {
                std::fs::copy(reuse.join("report.json"), first.join("report.json"))
                    .map_err(|error| error.to_string())?;
                let diff =
                    std::fs::read_to_string(first.join("context/PR_DIFF.patch")).map_err(|error| error.to_string())?;
                let validation = super::frozen::materialize(
                    &inputs,
                    &first.join("report.json"),
                    &first.join("context"),
                    &chunks[0].files,
                    &map.head_sha,
                    &diff,
                    &first.join("summary.md"),
                    &first.join("comments.json"),
                );
                results.push(super::diagnostics::restore_chunk(
                    &chunks[0],
                    &first,
                    validation,
                    true,
                    super::RunUsage::default(),
                )?);
                println!("REAL_REVIEW_PROGRESS {}", json!({"chunk":1,"cached":true,"newAcu":0}));
            }
        }
        if final_only && results.len() != chunks.len() {
            return Err(
                "Final-only validation requires every chunk to be restored and validated; no chunk inference allowed"
                    .into(),
            );
        }
        let batch = &chunks[results.len()..];
        let batch_results = super::run_review_chunk_batch(
            &inputs,
            &cancel,
            &mut progress,
            &root,
            &context,
            &map,
            batch,
            chunks.len(),
            &root.join("unused-config.json"),
            &map.head_sha,
            plan.prior_sha.as_deref().filter(|_| plan.incremental),
            &plan.reviewed_base_sha.clone().ok_or("Recorded base required")?,
            results.len(),
        )?;
        results.extend(batch_results);
        let mut output = if repair_final {
            repair_diagnostic_final(
                &inputs,
                &root,
                &map,
                &plan,
                packet["fullDiff"].as_str().unwrap(),
                &results,
                &cancel,
                &mut progress,
            )?
        } else {
            super::run_adaptive_final(
                &inputs,
                &cancel,
                &mut progress,
                &root,
                &context,
                &map,
                &plan,
                packet["fullDiff"].as_str().unwrap(),
                &map.head_sha,
                &root.join("unused-config.json"),
                &results,
            )?
        };
        super::diagnostics::retain_chunk_warnings(&mut output, &results);
        super::diagnostics::finish_audit(
            &inputs,
            &root,
            &output.body,
            output
                .comments_json
                .as_deref()
                .ok_or("Validated comments unavailable")?,
            packet["fullDiff"].as_str().unwrap(),
            &output.evidence_warnings,
        )?;
        Ok(output)
    })();
    let tokens: i64 = ledger
        .query_row(
            "SELECT COALESCE(SUM(input_tokens + output_tokens),0) FROM review_run_stages",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let summary = json!({"runId":packet["runId"],"status":if result.is_ok(){"complete"}else{"failed"},"error":result.as_ref().err(),"evidenceWarnings":result.as_ref().ok().map(|output|&output.evidence_warnings),"tokens":tokens,"chunks":chunks.len(),"published":false});
    std::fs::write(
        source.join("diagnostic-result.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("REAL_REVIEW_RESULT {summary}");
    assert!(
        result.is_ok(),
        "Real review did not finish; inspect the retained diagnostic result"
    );
}

#[test]
#[ignore = "Validates a retained real model report offline; never executes a model"]
fn validate_retained_real_chunk_report() {
    use serde_json::json;
    let source = std::path::PathBuf::from(
        std::env::var("OCR_DIAGNOSTIC_EXPORT_STAGE").expect("Retained diagnostic directory required"),
    );
    let directory = std::env::var("OCR_DIAGNOSTIC_STAGE")
        .map(|relative| {
            crate::sandbox::safe_path(&relative).unwrap();
            source.join(relative)
        })
        .unwrap_or_else(|_| source.clone());
    let packet: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("stage.json")).unwrap()).unwrap();
    let descriptor = serde_json::from_value(packet["descriptor"].clone()).unwrap();
    let archive = crate::review_archive::Archive::read(&source.join("snapshot.archive"), &descriptor).unwrap();
    let request: crate::sandbox::runner::Request = serde_json::from_value(packet["request"].clone()).unwrap();
    let crate::sandbox::runner::Request::Run { files, .. } = request else {
        panic!("Expected saved stage")
    };
    archive.validate_stage_files(&files).unwrap();
    let root = super::make_run_dir(-1358).unwrap();
    for (name, content) in &files {
        crate::sandbox::safe_path(name).unwrap();
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let stage: crate::review_archive::Stage = serde_json::from_str(&files["context/STAGE.json"]).unwrap();
    let report_path = directory.join("report.json");
    let result = crate::review::evidence::EvidenceResolver::frozen(&root.join("context"), &archive).materialize(
        &report_path,
        &stage.files,
        &archive.scope().head_sha,
        &files["context/PR_DIFF.patch"],
        &root.join("summary.md"),
        &root.join("comments.json"),
    );
    let raw = std::fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let diagnostics = report["findings"].as_array().unwrap().iter().enumerate().map(|(index, point)| {
        let path = point["path"].as_str().unwrap_or_default();
        let known_path = archive.entries().iter().any(|entry| entry.source.path == path);
        let empty_text = ["title","detail","action"].into_iter().filter(|key|point[*key].as_str().is_none_or(|text|text.trim().is_empty())).collect::<Vec<_>>();
        json!({"index":index,"kindValid":matches!(point["kind"].as_str(),Some("finding"|"question")),"emptyTextFields":empty_text,"inAssignedScope":stage.files.iter().any(|file|file==path),"knownAnchorPath":known_path.then_some(path),"evidenceCount":point["evidence"].as_array().map(Vec::len),"counterEvidenceCount":point["counterEvidence"].as_array().map(Vec::len)})
    }).collect::<Vec<_>>();
    let validation = json!({"runId":packet["runId"],"chunk":packet["chunk"],"valid":result.is_ok(),"error":result.as_ref().err(),"repairEligible":result.as_ref().err().is_some_and(|error|super::output_repair::repairable(error)),"reportHash":super::sha256_hex(&raw),"findings":diagnostics,"outputTokens":super::parse_transcript_usage(&directory.join("transcript.json")).output_tokens,"modelCalls":0});
    println!("REAL_REPORT_VALIDATION {validation}");
    std::fs::remove_dir_all(root).unwrap();
    assert!(result.is_ok(), "Retained model report failed validation");
}

#[test]
fn chunk_worker_count_is_bounded_independently_of_total_chunks() {
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.review_parallel_chunks = 20;
    assert_eq!(super::chunk_worker_count(&inputs, 95), 6);
    assert_eq!(super::chunk_worker_count(&inputs, 1), 1);
    assert_eq!(super::chunk_worker_count(&inputs, 0), 0);
    inputs.review_parallel_chunks = 2;
    assert_eq!(super::chunk_worker_count(&inputs, 95), 2);
    inputs.review_parallel_chunks = 0;
    assert_eq!(super::chunk_worker_count(&inputs, 95), 6);
}

#[test]
fn operational_preflight_streams_many_individually_bounded_stages() {
    let root = super::make_run_dir(-1101).unwrap();
    let context = root.join("context");
    std::fs::create_dir_all(context.join("skill")).unwrap();
    std::fs::write(context.join("PR.md"), "p".repeat(1_000_000)).unwrap();
    std::fs::write(context.join("skill/SKILL.md"), "Review").unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let scope = crate::review_archive::Scope {
        host: "github.com".into(),
        repository: "acme/fixture".into(),
        head_sha: "a".repeat(40),
        base_sha: "b".repeat(40),
        merge_base_sha: "c".repeat(40),
        prior_sha: None,
    };
    let source = crate::review_archive::SourceEntry {
        path: "a.rs".into(),
        revision: scope.head_sha.clone(),
        mode: "100644".into(),
        git_blob_id: "d".repeat(40),
        bytes: 2,
    };
    let archive =
        crate::review_archive::Archive::build(scope.clone(), vec![source], &cancel, |_| Ok(b"x\n".to_vec())).unwrap();
    archive
        .prepare_stage(&context, &["a.rs".into()], "", &scope.merge_base_sha)
        .unwrap();
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.frozen = Some(std::sync::Arc::new(super::frozen::FrozenContext::new(archive)));
    let stages = (0..40)
        .map(|index| {
            let directory = root.join(format!("chunks/{index:03}"));
            std::fs::create_dir_all(&directory).unwrap();
            let prompt = directory.join("prompt.md");
            std::fs::write(&prompt, "Review the prepared evidence").unwrap();
            (directory, context.clone(), prompt)
        })
        .collect::<Vec<_>>();
    for count in [12usize, 40] {
        let mut visited = 0usize;
        let mut bytes = 0usize;
        super::frozen::for_each_prepared_request(
            &inputs,
            &root,
            &stages[..count],
            "offline",
            1,
            "claude",
            &cancel,
            |index, request| {
                assert_eq!(index, visited);
                if let crate::sandbox::runner::Request::Run { files, .. } = request {
                    assert_eq!(files.get("context/PR.md").map(String::len), Some(1_000_000));
                    assert_eq!(files.get("context/changed/a.rs").map(String::as_str), Some("x\n"));
                    bytes += files
                        .iter()
                        .map(|(path, content)| path.len() + content.len())
                        .sum::<usize>();
                }
                visited += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(visited, count);
        assert!(bytes > crate::review::policy::MAX_EVIDENCE_BYTES);
    }
    assert!(
        super::frozen::for_each_prepared_request(&inputs, &root, &[], "offline", 1, "claude", &cancel, |_, _| Ok(()))
            .is_err()
    );
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut visited = 0usize;
    assert!(super::frozen::for_each_prepared_request(
        &inputs,
        &root,
        &stages,
        "offline",
        1,
        "claude",
        &cancel,
        |_, _| {
            visited += 1;
            Ok(())
        }
    )
    .is_err());
    assert_eq!(visited, 0);
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut visited = 0usize;
    let stopped =
        super::frozen::for_each_prepared_request(&inputs, &root, &stages, "offline", 1, "claude", &cancel, |_, _| {
            visited += 1;
            Err("fixture stop".into())
        })
        .unwrap_err();
    assert_eq!(stopped, "fixture stop");
    assert_eq!(visited, 1);
    let last_prompt = stages.last().unwrap().2.clone();
    std::fs::write(&last_prompt, "p".repeat(crate::review::policy::MAX_PROMPT_BYTES + 1)).unwrap();
    let mut visited = 0usize;
    assert!(super::frozen::for_each_prepared_request(
        &inputs,
        &root,
        &stages,
        "offline",
        1,
        "claude",
        &cancel,
        |_, _| {
            visited += 1;
            Ok(())
        }
    )
    .is_err());
    assert_eq!(visited, 39);
    std::fs::write(&last_prompt, "Review the prepared evidence").unwrap();
    std::fs::write(
        context.join("PR.md"),
        "p".repeat(crate::review::policy::MAX_EVIDENCE_BYTES + 1),
    )
    .unwrap();
    let mut visited = 0usize;
    assert!(super::frozen::for_each_prepared_request(
        &inputs,
        &root,
        &stages,
        "offline",
        1,
        "claude",
        &cancel,
        |_, _| {
            visited += 1;
            Ok(())
        }
    )
    .is_err());
    assert_eq!(visited, 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_turn_usage_counts_cached_input_separately() {
    let raw = "{\"type\":\"thread.started\"}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1200,\"cached_input_tokens\":200,\"output_tokens\":300}}\n";
    let mut per_model = std::collections::HashMap::new();
    collect_usage(&super::transcript_events(raw).unwrap(), &mut per_model);
    let usage = finalize_usage(per_model);
    assert_eq!(
        (usage.input_tokens, usage.cache_read_tokens, usage.output_tokens),
        (1000, 200, 300)
    );
    assert_eq!(usage.model.as_deref(), Some("codex"));
}

#[test]
fn opencode_step_finish_events_sum_tokens() {
    let raw = [
        r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":800,"output":150,"reasoning":50,"cache":{"read":120,"write":30}},"cost":0.01}}"#,
        r#"{"type":"text","part":{"type":"text","text":"{}"}}"#,
        r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":200,"output":100,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.002}}"#,
    ].join("\n");
    let mut per_model = std::collections::HashMap::new();
    collect_usage(&super::transcript_events(&raw).unwrap(), &mut per_model);
    let usage = finalize_usage(per_model);
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens
        ),
        (1000, 300, 120, 30)
    );
    assert_eq!(usage.model.as_deref(), Some("opencode"));
}
