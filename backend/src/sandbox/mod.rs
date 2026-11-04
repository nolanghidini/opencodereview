use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};

mod egress;
pub(crate) mod runner;

pub use egress::serve as serve_egress;
pub use runner::guard_tool;
pub use runner::serve as serve_runner;

pub const MAX_STAGE_BYTES: usize = 8 * 1024 * 1024;

pub fn engine_version() -> String {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(executor_fingerprint).clone()
}

fn executor_fingerprint() -> String {
    let mut hash = Sha256::new();
    for source in [
        include_bytes!("../review/mod.rs").as_slice(),
        include_bytes!("mod.rs").as_slice(),
        include_bytes!("runner.rs").as_slice(),
        include_bytes!("read_commands.rs").as_slice(),
        include_bytes!("../review/evidence.rs").as_slice(),
        include_bytes!("../review/warnings.rs").as_slice(),
        include_bytes!("../review_archive/mod.rs").as_slice(),
        include_bytes!("../review_archive/manifest.rs").as_slice(),
        include_bytes!("../review_archive/dependencies.rs").as_slice(),
        include_bytes!("../review/dependencies.rs").as_slice(),
        include_bytes!("../review_archive/git.rs").as_slice(),
        include_bytes!("../review_archive/transfer.rs").as_slice(),
        include_bytes!("../review_archive/store.rs").as_slice(),
        include_bytes!("../review_archive/stage.rs").as_slice(),
        include_bytes!("../review/frozen.rs").as_slice(),
        include_bytes!("../review/integration/mod.rs").as_slice(),
        include_bytes!("../providers/claude.rs").as_slice(),
        include_bytes!("../providers/codex.rs").as_slice(),
        include_bytes!("../providers/opencode.rs").as_slice(),
        include_bytes!("../indexer/mod.rs").as_slice(),
        include_bytes!("../review/scope.rs").as_slice(),
        include_bytes!("../review/policy.rs").as_slice(),
        include_bytes!("../review/recovery.rs").as_slice(),
        include_bytes!("../review/output_repair.rs").as_slice(),
    ] {
        hash.update(source);
    }
    format!(
        "{}:{}:{:x}",
        crate::review::REVIEW_PROMPT_VERSION,
        crate::db::INDEX_SPEC_VERSION,
        hash.finalize()
    )
}

pub fn safe_path(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value.contains('\0')
        || value.split('/').any(|p| p.is_empty() || p == "." || p == "..")
        || Path::new(value)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Unsafe snapshot path".into());
    }
    Ok(())
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn secret() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(digest(&bytes))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn archive() -> crate::review_archive::Archive {
        let scope = crate::review_archive::Scope {
            host: "github.com".into(),
            repository: "acme/repo".into(),
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            merge_base_sha: "b".repeat(40),
            prior_sha: None,
        };
        crate::review_archive::Archive::build(
            scope,
            vec![crate::review_archive::SourceEntry {
                path: "src/a.ts".into(),
                revision: "a".repeat(40),
                mode: "100644".into(),
                git_blob_id: "d".repeat(40),
                bytes: 4,
            }],
            &std::sync::atomic::AtomicBool::new(false),
            |_| Ok(b"test".to_vec()),
        )
        .unwrap()
    }

    #[test]
    fn rejects_unsafe_snapshot_paths() {
        assert!(safe_path("src/a.ts").is_ok());
        for path in ["../a", "/etc/passwd", "a/../b", "a\\b", "a//b", "./a"] {
            assert!(safe_path(path).is_err(), "{path}");
        }
    }
}
