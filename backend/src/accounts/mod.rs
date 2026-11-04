//! Credentials of the AI accounts listed in `ai_accounts`. Each account owns one directory,
//! `$OCR_DATA_DIR/accounts/<id>/`, holding its provider's credential file:
//! `claude/oauth-token`, `codex/auth.json` or
//! `opencode/auth.json`.
pub(crate) mod sandbox_sync;
pub(crate) mod usage;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::db::Database;

pub(crate) fn root() -> PathBuf {
    std::env::var_os("OCR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "data".into())
        .join("accounts")
}

pub(crate) fn claude_token_path(id: i64) -> PathBuf {
    root().join(id.to_string()).join("claude/oauth-token")
}

/// Ids of accounts that have a credential directory, in ascending order.
pub(crate) fn stored_ids() -> Vec<i64> {
    let mut ids = fs::read_dir(root())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

pub(crate) fn codex_auth_path(id: i64) -> PathBuf {
    root().join(id.to_string()).join("codex/auth.json")
}

pub(crate) fn opencode_auth_path(id: i64) -> PathBuf {
    root().join(id.to_string()).join("opencode/auth.json")
}

pub(crate) fn store_claude_token(id: i64, token: &str) -> Result<(), String> {
    store(&claude_token_path(id), id, token.as_bytes())
}

/// Saves a Codex `auth.json`, from a sign-in or refreshed by a review stage.
pub(crate) fn store_codex_auth(id: i64, raw: &str) -> Result<(), String> {
    crate::providers::codex::validate_auth(raw)?;
    store(&codex_auth_path(id), id, raw.as_bytes())
}

/// Saves an OpenCode account's plan, Go key and model.
pub(crate) fn store_opencode_auth(id: i64, raw: &str) -> Result<(), String> {
    crate::providers::opencode::validate_auth(raw)?;
    store(&opencode_auth_path(id), id, raw.as_bytes())
}

fn store(path: &std::path::Path, id: i64, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid credential location")?;
    fs::create_dir_all(parent).map_err(|_| "Credential location is not writable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [parent.to_path_buf(), root().join(id.to_string())] {
            let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
        }
    }
    let temporary = parent.join(format!(".credential-{}", crate::sandbox::secret()?));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|_| file.sync_all()))
        .and_then(|_| fs::rename(&temporary, path))
        .map_err(|_| "Could not persist the account sign-in".to_string());
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    sandbox_sync::sync_app_auth()
}

pub(crate) fn remove(id: i64) -> Result<(), String> {
    match fs::remove_dir_all(root().join(id.to_string())) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not remove the account credentials".into()),
    }
    sandbox_sync::sync_app_auth()
}

pub(crate) fn has_credential(provider: &str, id: i64) -> bool {
    match provider {
        "claude" => crate::providers::claude::read_token(&claude_token_path(id)).is_ok(),
        "codex" => fs::read_to_string(codex_auth_path(id))
            .is_ok_and(|raw| crate::providers::codex::validate_auth(&raw).is_ok()),
        "opencode" => fs::read_to_string(opencode_auth_path(id))
            .is_ok_and(|raw| crate::providers::opencode::validate_auth(&raw).is_ok()),
        _ => false,
    }
}

/// Removes accounts of providers the app no longer runs (Gemini), and their sign-ins. The sign-ins
/// are found by their provider folder, not by the removed rows: a database that is still empty,
/// such as one waiting for a SQLite import, must not cost other accounts their sign-ins, and a
/// folder that could not be removed is tried again on the next start.
pub(crate) fn remove_unsupported(db: &Database) -> Result<(), String> {
    db.remove_unsupported_ai_accounts()?;
    if remove_unsupported_sign_ins(&root())? {
        sandbox_sync::sync_app_auth()?;
    }
    Ok(())
}

/// Deletes the Gemini folders under `root`, and each account folder they leave empty. Returns
/// whether it deleted any.
fn remove_unsupported_sign_ins(root: &std::path::Path) -> Result<bool, String> {
    let mut removed = false;
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(false);
    };
    for account in entries.flatten().map(|entry| entry.path()) {
        match fs::remove_dir_all(account.join("gemini")) {
            Ok(()) => {
                removed = true;
                let _ = fs::remove_dir(&account);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Could not remove the sign-in of a removed account".into()),
        }
    }
    Ok(removed)
}

/// Installs from before multiple accounts kept one token at the app path; it becomes the first
/// Claude account so reviews keep working after the upgrade.
pub(crate) fn adopt_legacy_token(db: &Database) -> Result<(), String> {
    let legacy = crate::providers::claude::app_token_path();
    let Ok(token) = crate::providers::claude::read_token(&legacy) else {
        return Ok(());
    };
    if db
        .list_ai_accounts()?
        .iter()
        .any(|account| account.provider == "claude")
    {
        return Ok(());
    }
    let account = db.create_ai_account("claude", "Claude")?;
    store_claude_token(account.id, &token)?;
    fs::remove_file(&legacy).map_err(|_| "Could not retire the previous Claude token location".to_string())?;
    sandbox_sync::sync_app_auth()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_removed_providers_lose_their_sign_ins() {
        let root = std::env::temp_dir().join(format!("ocr-unsupported-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for path in ["1/claude", "2/codex", "3/gemini", "4/opencode"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
            std::fs::write(root.join(path).join("secret"), "x").unwrap();
        }
        assert!(super::remove_unsupported_sign_ins(&root).unwrap());
        assert!(root.join("1/claude/secret").exists());
        assert!(root.join("2/codex/secret").exists());
        assert!(root.join("4/opencode/secret").exists());
        assert!(!root.join("3").exists());
        // Nothing left to remove, and a missing root is not an error.
        assert!(!super::remove_unsupported_sign_ins(&root).unwrap());
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!super::remove_unsupported_sign_ins(&root).unwrap());
    }
}
