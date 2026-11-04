use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static SYNC: Mutex<()> = Mutex::new(());
const MAX_AUTH_BYTES: u64 = 128 * 1024;

/// Projects every account's credential into the runners' read-only volume as
/// `accounts/<id>/<provider>/<credential file>` (see [`crate::accounts`]), and removes
/// projections of deleted accounts.
pub(crate) fn sync_app_auth() -> Result<(), String> {
    let _guard = SYNC.lock().map_err(|_| "App authentication sync unavailable")?;
    let Some(target) = std::env::var_os("OCR_REVIEW_CLAUDE_AUTH_DIR").map(PathBuf::from) else {
        return Ok(());
    };
    private_dir(&target)?;
    // The single pre-accounts token is no longer projected.
    match fs::remove_file(target.join("claude/oauth-token")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not clear the previous app authentication".into()),
    }
    let ids = crate::accounts::stored_ids();
    for id in &ids {
        mirror(
            &crate::accounts::claude_token_path(*id),
            &target.join(format!("accounts/{id}/claude/oauth-token")),
        )?;
        mirror(
            &crate::accounts::codex_auth_path(*id),
            &target.join(format!("accounts/{id}/codex/auth.json")),
        )?;
        mirror(
            &crate::accounts::opencode_auth_path(*id),
            &target.join(format!("accounts/{id}/opencode/auth.json")),
        )?;
    }
    if let Ok(entries) = fs::read_dir(target.join("accounts")) {
        for entry in entries.flatten() {
            let stale = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i64>().ok())
                .is_none_or(|id| !ids.contains(&id));
            if stale && !fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Authentication destination must not be a symlink".into());
    }
    fs::create_dir_all(path).map_err(|_| "Authentication volume is unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Authentication directory permissions could not be secured")?;
    }
    Ok(())
}

fn mirror(source: &Path, target: &Path) -> Result<(), String> {
    let result = mirror_inner(source, target);
    if result.is_err() {
        let _ = fs::remove_file(target);
    }
    result
}

fn mirror_inner(source: &Path, target: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::remove_file(target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Could not clear disconnected app authentication".into()),
            }
            return Ok(());
        }
        Err(_) => return Err("App authentication could not be read".into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_AUTH_BYTES {
        return Err("App authentication must be a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(source)
        .and_then(|file| file.take(MAX_AUTH_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| "App authentication could not be read")?;
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err("App authentication exceeds the byte budget".into());
    }
    let parent = target.parent().ok_or("Invalid authentication destination")?;
    private_dir(parent)?;
    if fs::symlink_metadata(target).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("Authentication destination must be a regular file".into());
    }
    if fs::metadata(target).is_ok_and(|metadata| metadata.len() == bytes.len() as u64)
        && fs::read(target).is_ok_and(|existing| existing == bytes)
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target, fs::Permissions::from_mode(0o600))
                .map_err(|_| "Authentication file permissions could not be secured")?;
        }
        return Ok(());
    }
    let temporary = parent.join(format!(".auth-{}", crate::sandbox::secret()?));
    let result: Result<(), String> = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Authentication volume is not writable")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Could not persist app authentication")?;
        fs::rename(&temporary, target).map_err(|_| "Could not publish app authentication".to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_auth_projects_only_explicit_files_and_tracks_app_logout() {
        let root = std::env::temp_dir().join(format!("ocr-auth-test-{}", crate::sandbox::secret().unwrap()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("app-credential");
        let target = root.join("private/claude/oauth-token");
        fs::write(&source, "fixture-auth").unwrap();
        fs::write(root.join("operational.sqlite"), "not-authentication").unwrap();
        mirror(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "fixture-auth");
        assert!(!root.join("private/operational.sqlite").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                fs::metadata(target.parent().unwrap()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::write(&source, "refreshed-fixture").unwrap();
        mirror(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "refreshed-fixture");
        fs::remove_file(&source).unwrap();
        mirror(&source, &target).unwrap();
        assert!(!target.exists());
        assert!(root.join("operational.sqlite").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_auth_refuses_symlinks_and_oversized_files() {
        let root = std::env::temp_dir().join(format!("ocr-auth-test-{}", crate::sandbox::secret().unwrap()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let link = root.join("link");
        fs::write(&source, "fixture").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(mirror(&link, &root.join("target")).is_err());
        mirror(&source, &root.join("target")).unwrap();
        fs::write(&source, vec![0; MAX_AUTH_BYTES as usize + 1]).unwrap();
        assert!(mirror(&source, &root.join("target")).is_err());
        assert!(!root.join("target").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
