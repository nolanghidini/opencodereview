use super::*;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::{
    fs::{OpenOptionsExt, PermissionsExt},
    net::{UnixListener, UnixStream},
    process::CommandExt,
};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[path = "read_commands.rs"]
mod read_commands;

pub(crate) const SNAPSHOT_READ_INSTRUCTIONS: &str = read_commands::INSTRUCTIONS;

const MAX_FRAME: usize = 48 * 1024 * 1024;
const UID: u32 = 10001;
const SESSION: &str = "/scratch/session";
const CLI: &str = "/opt/claude/bin/claude";
const CODEX_CLI: &str = "/opt/codex/bin/codex";
const TRANSCRIPT: &str = "/scratch/transcript.jsonl";
/// Codex keeps its home, sign-in and final message here, owned by the model UID.
const CODEX_HOME: &str = "/scratch/session/codex";
const OPENCODE_CLI: &str = "/opt/opencode/bin/opencode";
/// OpenCode keeps its XDG homes and configuration here; its Go key, if any, arrives in the environment.
const OPENCODE_HOME: &str = "/scratch/session/opencode";
const PROVIDERS: &[&str] = &["claude", "codex", "opencode"];

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub(crate) enum Request {
    Catalog {},
    Install {
        descriptor: crate::review_archive::Descriptor,
        manifest: String,
        engine_version: String,
    },
    Blob {
        hash: String,
        data: String,
    },
    Seal {},
    Run {
        model: String,
        version: String,
        prompt: String,
        files: BTreeMap<String, String>,
        #[serde(default)]
        archive_id: Option<String>,
        #[serde(default)]
        preflight: bool,
        account: i64,
        #[serde(default = "claude")]
        provider: String,
        /// The reasoning effort for `model`; empty means the model's default.
        #[serde(default)]
        effort: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Response {
    Catalog {
        models: Vec<String>,
        version: String,
    },
    Ack,
    Ready,
    Started,
    Pulse,
    /// `credential` is a Codex sign-in the CLI refreshed during the stage, for the broker to keep.
    Finished {
        report: Option<String>,
        decisions: Option<String>,
        transcript: Option<String>,
        error: Option<String>,
        #[serde(default)]
        credential: Option<String>,
    },
    Error {
        error: String,
    },
}

fn claude() -> String {
    "claude".into()
}

fn send<T: Serialize>(stream: &mut impl Write, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("Runner frame exceeds byte budget".into());
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|_| stream.write_all(&bytes))
        .map_err(|_| "Runner IPC disconnected".into())
}

fn receive<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> Result<T, String> {
    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .map_err(|_| "Runner IPC disconnected or timed out")?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("Runner frame exceeds byte budget".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).map_err(|_| "Truncated runner frame")?;
    serde_json::from_slice(&bytes).map_err(|_| "Invalid runner frame".into())
}

#[cfg(unix)]
fn connect_at(socket: &Path) -> Result<UnixStream, String> {
    let stream =
        UnixStream::connect(socket).map_err(|_| "Isolation runtime unavailable: runner socket disconnected")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|e| e.to_string())?;
    Ok(stream)
}

#[cfg(unix)]
#[cfg(test)]
pub(crate) fn catalog_at(socket: &Path) -> Result<(Vec<String>, String), String> {
    catalog_stream(connect_at(socket)?)
}

/// Whether nothing listens at `socket` any more; a socket a replaced runner left behind is removed.
#[cfg(unix)]
pub(crate) fn is_stale(socket: &Path) -> bool {
    match UnixStream::connect(socket) {
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            let _ = fs::remove_file(socket);
            true
        }
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => false,
    }
}

/// The catalogue of the runner at `socket`, or `None` when nothing listens there any more: a
/// replaced runner leaves its socket behind, which is removed.
#[cfg(unix)]
pub(crate) fn catalog_or_stale(socket: &Path) -> Result<Option<(Vec<String>, String)>, String> {
    match UnixStream::connect(socket) {
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            let _ = fs::remove_file(socket);
            Ok(None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Isolation runtime unavailable: runner socket disconnected".into()),
        Ok(stream) => {
            stream
                .set_read_timeout(Some(Duration::from_secs(40)))
                .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(10))))
                .map_err(|e| e.to_string())?;
            catalog_stream(stream).map(Some)
        }
    }
}

#[cfg(unix)]
fn catalog_stream(mut stream: UnixStream) -> Result<(Vec<String>, String), String> {
    send(&mut stream, &Request::Catalog {})?;
    match receive(&mut stream)? {
        Response::Catalog { models, version } => Ok((models, version)),
        Response::Error { error } => Err(error),
        _ => Err("Runner catalogue returned an unexpected response".into()),
    }
}

pub(crate) fn stage_request(
    model: &str,
    version: &str,
    account: i64,
    provider: &str,
    root: &Path,
    stage: &Path,
    context: &Path,
    prompt: &Path,
) -> Result<Request, String> {
    let mut files = BTreeMap::new();
    let mut pending = vec![context.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                pending.push(entry.map_err(|e| e.to_string())?.path());
            }
        } else if meta.is_file() {
            let name = format!(
                "context/{}",
                path.strip_prefix(context)
                    .map_err(|_| "Context escaped broker workspace")?
                    .to_str()
                    .ok_or("Invalid context path")?
            );
            safe_path(&name)?;
            files.insert(name, fs::read_to_string(&path).map_err(|e| e.to_string())?);
        } else {
            return Err("Non-regular snapshot evidence".into());
        }
    }
    let stage_name = stage
        .strip_prefix(root)
        .map_err(|_| "Stage escaped broker workspace")?
        .to_str()
        .ok_or("Invalid stage path")?;
    for name in [
        "chunk_findings.json",
        "chunk_summaries.md",
        "rejected-report.json",
        "rejected-decisions.json",
    ] {
        let path = stage.join(name);
        if path.is_file() {
            files.insert(
                format!("integration/{name}"),
                fs::read_to_string(path).map_err(|e| e.to_string())?,
            );
        }
    }
    let mut prompt = fs::read_to_string(prompt)
        .map_err(|e| e.to_string())?
        .replace(root.to_str().ok_or("Invalid workspace root")?, "/work");
    let context_prefix = format!(
        "/work/{}",
        context
            .strip_prefix(root)
            .map_err(|_| "Invalid context root")?
            .display()
    );
    prompt = prompt
        .replace(&context_prefix, "/work/context")
        .replace("and chunk_summaries.md", "and /work/integration/chunk_summaries.md");
    let prefix = if stage_name.is_empty() {
        "/work".to_string()
    } else {
        format!("/work/{stage_name}")
    };
    for name in ["report.json", "finding_decisions.json"] {
        prompt = prompt.replace(&format!("{prefix}/{name}"), &format!("/output/{name}"));
    }
    for name in [
        "chunk_findings.json",
        "chunk_summaries.md",
        "rejected-report.json",
        "rejected-decisions.json",
    ] {
        prompt = prompt.replace(&format!("{prefix}/{name}"), &format!("/work/integration/{name}"));
    }
    let archive_id = files
        .get("context/FROZEN.json")
        .map(|raw| serde_json::from_str::<crate::review_archive::Descriptor>(raw).map(|descriptor| descriptor.id))
        .transpose()
        .map_err(|_| "Invalid frozen descriptor")?;
    let request = Request::Run {
        model: model.into(),
        version: version.into(),
        prompt,
        files,
        archive_id,
        preflight: false,
        account,
        provider: provider.into(),
        effort: String::new(),
    };
    validate(&request)?;
    Ok(request)
}

/// Points a stage request at `provider`'s model and effort, as chosen in Settings.
pub(crate) fn set_engine(request: &mut Request, model: &str, effort: &str) -> Result<(), String> {
    if let Request::Run {
        model: current,
        effort: current_effort,
        ..
    } = request
    {
        *current = model.into();
        *current_effort = effort.into();
    }
    validate(request)
}

fn validate(request: &Request) -> Result<(), String> {
    if let Request::Run {
        model,
        version,
        prompt,
        files,
        archive_id,
        account,
        provider,
        ..
    } = request
    {
        if *account <= 0 || !PROVIDERS.contains(&provider.as_str()) {
            return Err("Invalid account identity".into());
        }
        if archive_id
            .as_ref()
            .is_some_and(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("Invalid archive identity".into());
        }
        if model.is_empty()
            || model.len() > 120
            || !model.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || version.is_empty()
            || version.len() > 256
            || prompt.len() > 200_000
            || files.len() > 5000
            || files.iter().map(|(p, s)| p.len() + s.len()).sum::<usize>() > MAX_STAGE_BYTES
        {
            return Err("Invalid runner stage budget or identity".into());
        }
        for name in files.keys() {
            safe_path(name)?;
            if name
                .split('/')
                .any(|part| matches!(part, ".devin" | ".agents" | ".claude" | ".opencode" | ".git"))
            {
                return Err("Executable configuration is not snapshot evidence".into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn execute_archive_at(
    socket: &Path,
    archive: &crate::review_archive::Archive,
    request: &Request,
    stage: &Path,
    transcript: &Path,
    cancel: &AtomicBool,
    on_start: &mut dyn FnMut(),
) -> Result<(), String> {
    use base64::Engine;
    validate(request)?;
    if !matches!(request, Request::Run { archive_id: Some(id), .. } if *id == archive.descriptor().id) {
        return Err("Runner request is not bound to its archive".into());
    }
    let mut stream = connect_at(socket)?;
    let started = Instant::now();
    let mut transmit = |request: &Request| -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if started.elapsed() > Duration::from_secs(300) {
            return Err("Archive transfer timed out".into());
        }
        send(&mut stream, request)?;
        match receive(&mut stream)? {
            Response::Ack => Ok(()),
            Response::Error { error } => Err(error),
            _ => Err("Invalid archive transfer response".into()),
        }
    };
    transmit(&Request::Install {
        engine_version: crate::sandbox::engine_version(),
        descriptor: archive.descriptor(),
        manifest: String::from_utf8(archive.manifest_bytes().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
    })?;
    for (hash, bytes) in archive.transfer_blobs() {
        transmit(&Request::Blob {
            hash: hash.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        })?;
    }
    transmit(&Request::Seal {})?;
    exchange(stream, request, stage, transcript, cancel, on_start)
}

#[cfg(unix)]
pub(crate) fn preflight_at(
    socket: &Path,
    archive: &crate::review_archive::Archive,
    request: &Request,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut request = request.clone();
    let Request::Run { preflight, .. } = &mut request else {
        return Err("Expected a stage request".into());
    };
    *preflight = true;
    execute_archive_at(
        socket,
        archive,
        &request,
        Path::new("/unused"),
        Path::new("/unused/transcript"),
        cancel,
        &mut || {},
    )
}

#[cfg(unix)]
fn exchange(
    mut stream: UnixStream,
    request: &Request,
    stage: &Path,
    transcript_path: &Path,
    cancel: &AtomicBool,
    on_start: &mut dyn FnMut(),
) -> Result<(), String> {
    send(&mut stream, request)?;
    let started = Instant::now();
    let mut running = false;
    let mut cancelling = false;
    loop {
        if cancel.load(Ordering::Relaxed) && !cancelling {
            stream
                .write_all(&[1])
                .map_err(|_| "Runner cancellation channel disconnected")?;
            cancelling = true;
        }
        if started.elapsed() > Duration::from_secs(950) {
            return Err("Runner timed out; connection closed".into());
        }
        match receive(&mut stream)? {
            Response::Ready if matches!(request, Request::Run { preflight: true, .. }) && !running => return Ok(()),
            Response::Started if !running && !matches!(request, Request::Run { preflight: true, .. }) => {
                running = true;
                on_start();
            }
            Response::Pulse if running => {}
            Response::Finished {
                report,
                decisions,
                transcript,
                error,
                credential,
            } if running => {
                if let (Some(credential), Request::Run { account, provider, .. }) = (credential, request) {
                    if provider.as_str() == "codex" {
                        crate::accounts::store_codex_auth(*account, &credential)?;
                    }
                }
                for (path, value, limit) in [
                    (stage.join("report.json"), report, 2_000_000),
                    (stage.join("finding_decisions.json"), decisions, 2_000_000),
                    (transcript_path.to_path_buf(), transcript, 32_000_000),
                ] {
                    if let Some(value) = value {
                        if value.len() > limit {
                            return Err("Runner output exceeds byte budget".into());
                        }
                        fs::write(path, value).map_err(|e| e.to_string())?;
                    }
                }
                return error.map_or(Ok(()), Err);
            }
            Response::Error { error } => return Err(error),
            _ => return Err("Unexpected runner protocol state".into()),
        }
    }
}

#[cfg(unix)]
pub(crate) fn private_dir(path: &Path, mode: u32) -> Result<(), String> {
    fs::create_dir_all(path)
        .and_then(|_| fs::set_permissions(path, fs::Permissions::from_mode(mode)))
        .map_err(|e| e.to_string())?;
    if path.starts_with("/work") {
        for directory in path.ancestors().take_while(|directory| directory.starts_with("/work")) {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn session(account: i64) -> Result<Command, String> {
    session_from_config(session_config(), account)
}

#[cfg(unix)]
fn session_from_config(settings: serde_json::Value, account: i64) -> Result<Command, String> {
    let root = Path::new(SESSION);
    private_dir(root, 0o755)?;
    private_dir(&root.join(".claude"), 0o700)?;
    std::os::unix::fs::chown(root.join(".claude"), Some(UID), Some(UID)).map_err(|e| e.to_string())?;
    let path = root.join("settings.json");
    if fs::symlink_metadata(&path).is_ok() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    fs::write(&path, settings.to_string())
        .and_then(|_| fs::set_permissions(&path, fs::Permissions::from_mode(0o444)))
        .map_err(|e| e.to_string())?;
    let auth_root = std::env::var("OCR_RUNNER_AUTH_ROOT").unwrap_or_else(|_| "/claude-auth/app-managed".into());
    let token = crate::providers::claude::read_token(
        &Path::new(&auth_root).join(format!("accounts/{account}/claude/oauth-token")),
    )
    .map_err(|_| crate::providers::claude::Failure::Auth.encode())?;
    let mut command = cli_command();
    crate::providers::claude::isolate(&mut command, root, &token);
    Ok(command)
}

/// A Codex execution on `account`, with its sign-in copied into a home only the model UID owns.
/// Returns the command and the sign-in it started from.
#[cfg(unix)]
fn codex_session(account: i64, model: &str, effort: &str) -> Result<(Command, String), String> {
    let auth_root = std::env::var("OCR_RUNNER_AUTH_ROOT").unwrap_or_else(|_| "/claude-auth/app-managed".into());
    let auth = read_output(
        &Path::new(&auth_root).join(format!("accounts/{account}/codex/auth.json")),
        crate::providers::codex::MAX_AUTH_BYTES as u64,
    )
    .ok()
    .flatten()
    .filter(|raw| crate::providers::codex::validate_auth(raw).is_ok())
    .ok_or_else(|| crate::providers::claude::Failure::Auth.encode())?;
    let home = Path::new(CODEX_HOME);
    private_dir(Path::new(SESSION), 0o755)?;
    private_dir(home, 0o700)?;
    std::os::unix::fs::chown(home, Some(UID), Some(UID)).map_err(|e| e.to_string())?;
    let path = home.join("auth.json");
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(auth.as_bytes()))
        .map_err(|e| e.to_string())?;
    std::os::unix::fs::chown(&path, Some(UID), Some(UID)).map_err(|e| e.to_string())?;
    let mut command = Command::new(CODEX_CLI);
    let proxy = std::env::var("OCR_RUNNER_PROXY").unwrap_or_else(|_| "http://review-egress:8790".into());
    command
        .env_clear()
        .env("PATH", "/opt/codex/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", home)
        .env("CODEX_HOME", home)
        .env("TMPDIR", "/tmp")
        .env("HTTPS_PROXY", &proxy)
        .env("HTTP_PROXY", &proxy)
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "error")
        .current_dir("/work")
        .uid(UID)
        .gid(UID)
        .process_group(0)
        .args(crate::providers::codex::exec_args(
            Path::new("/work"),
            &home.join("last-message.txt"),
            model,
            effort,
        ));
    Ok((command, auth))
}

/// Writes `contents` to a new file with `mode`, owned by the model UID when `owned`.
#[cfg(unix)]
fn session_file(path: &Path, contents: &str, mode: u32, owned: bool) -> Result<(), String> {
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .map_err(|e| e.to_string())?;
    if owned {
        std::os::unix::fs::chown(path, Some(UID), Some(UID)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// An OpenCode execution on `account`: its OpenCode Go key, if any, goes in the environment and its homes are
/// private to the model UID. Returns the command and the account it started from.
#[cfg(unix)]
fn opencode_session(account: i64) -> Result<(Command, String), String> {
    let auth_root = std::env::var("OCR_RUNNER_AUTH_ROOT").unwrap_or_else(|_| "/claude-auth/app-managed".into());
    let auth = read_output(
        &Path::new(&auth_root).join(format!("accounts/{account}/opencode/auth.json")),
        crate::providers::opencode::MAX_AUTH_BYTES as u64,
    )
    .ok()
    .flatten()
    .ok_or_else(|| crate::providers::claude::Failure::Auth.encode())?;
    let (provider, key, model) =
        crate::providers::opencode::parts(&auth).ok_or_else(|| crate::providers::claude::Failure::Auth.encode())?;
    let home = Path::new(OPENCODE_HOME);
    private_dir(Path::new(SESSION), 0o755)?;
    for name in ["", "data", "config", "cache", "state"] {
        let directory = home.join(name);
        private_dir(&directory, 0o700)?;
        std::os::unix::fs::chown(&directory, Some(UID), Some(UID)).map_err(|e| e.to_string())?;
    }
    let config = home.join("opencode.json");
    session_file(
        &config,
        &crate::providers::opencode::config(&model).to_string(),
        0o444,
        false,
    )?;
    let mut command = Command::new(OPENCODE_CLI);
    let proxy = std::env::var("OCR_RUNNER_PROXY").unwrap_or_else(|_| "http://review-egress:8790".into());
    command
        .env_clear()
        .env("PATH", "/opt/opencode/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", home)
        .env("TMPDIR", "/tmp")
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("HTTPS_PROXY", &proxy)
        .env("HTTP_PROXY", &proxy)
        .env("NO_COLOR", "1")
        .env("OPENCODE_CONFIG", &config)
        .envs(crate::providers::opencode::QUIET_ENV.iter().copied())
        .current_dir("/work")
        .uid(UID)
        .gid(UID)
        .process_group(0)
        .args(crate::providers::opencode::exec_args());
    if let Some(key) = key {
        command.env(provider.env, key);
    }
    Ok((command, auth))
}

/// Inference arguments for an isolated stage; the prompt is supplied on stdin.
#[cfg(unix)]
pub(crate) fn inference(command: &mut Command, model: &str, effort: &str) -> Result<(), String> {
    let (model, effort) = crate::providers::engines::resolve("claude", model, effort)?;
    command.args(crate::providers::claude::print_args(
        &Path::new(SESSION).join("settings.json"),
        crate::providers::claude::RUNNER_TOOLS,
        &model,
        &effort,
    ));
    Ok(())
}

#[cfg(unix)]
pub(crate) fn cli_version() -> Result<String, String> {
    let output = bounded(
        cli_command().arg("--version"),
        Path::new("/scratch"),
        &AtomicBool::new(false),
        15,
    )?;
    let version = String::from_utf8(output).map_err(|_| "Invalid CLI version")?;
    let version = version.split_whitespace().next().ok_or("Invalid CLI version")?;
    if version.is_empty() || version.len() > 64 || !version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.') {
        return Err("Invalid CLI version".into());
    }
    Ok(version.into())
}

fn session_config() -> serde_json::Value {
    crate::providers::claude::runner_settings("/usr/local/bin/ocr-sandbox tool-guard || exit 2", Vec::new())
}

const READ_ROOTS: &[&str] = &["/work", "/evidence/snapshot", "/output"];
const OUTPUTS: &[&str] = &["/output/report.json", "/output/finding_decisions.json"];

fn lexical(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (path.is_absolute()
        && path
            .components()
            .all(|part| !matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir)))
    .then_some(path)
}

pub fn guard_tool(input: impl Read) -> Result<(), String> {
    let mut raw = Vec::new();
    input
        .take(8_388_609)
        .read_to_end(&mut raw)
        .map_err(|_| "Tool policy input unavailable")?;
    if raw.len() > 8_388_608 {
        return Err("Tool policy input exceeds its limit".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| "Invalid tool policy input")?;
    if value["hook_event_name"] != "PreToolUse" {
        return Err("Frozen reviews permit only file tools and constrained snapshot reads".into());
    }
    let input = &value["tool_input"];
    let within = |key: &str, roots: &[&str], optional: bool| match input.get(key).and_then(|path| path.as_str()) {
        None if optional && input.get(key).is_none_or(|path| path.is_null()) => true,
        Some(path) => lexical(path).is_some_and(|path| roots.iter().any(|root| path.starts_with(root))),
        None => false,
    };
    let allowed = match value["tool_name"].as_str() {
        Some("Bash") => {
            return read_commands::validate_input(input, &[Path::new("/work"), Path::new("/evidence/snapshot")])
        }
        Some("Read") => within("file_path", READ_ROOTS, false),
        Some("Grep" | "Glob") => within("path", READ_ROOTS, true),
        Some("Write" | "Edit") => input["file_path"].as_str().is_some_and(|path| OUTPUTS.contains(&path)),
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err("Frozen reviews permit only file tools and constrained snapshot reads".into())
    }
}

#[cfg(unix)]
fn cli_command() -> Command {
    let mut command = Command::new(CLI);
    let proxy = std::env::var("OCR_RUNNER_PROXY").unwrap_or_else(|_| "http://review-egress:8790".into());
    command
        .env_clear()
        .env("PATH", "/opt/claude/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", SESSION)
        .env("TMPDIR", "/tmp")
        .env("HTTPS_PROXY", &proxy)
        .env("HTTP_PROXY", &proxy)
        .current_dir("/output")
        .uid(UID)
        .gid(UID)
        .process_group(0);
    command
}

#[cfg(unix)]
pub(crate) fn read_output(path: &Path, limit: u64) -> Result<Option<String>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Runner output unavailable".into()),
    };
    if !metadata.is_file() || metadata.len() > limit {
        return Err("Runner output is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "Runner output unavailable")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Runner output unavailable")?;
    if bytes.len() as u64 > limit {
        return Err("Runner output exceeds byte budget".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "Runner output is not UTF8".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalResponse {
    report: serde_json::Value,
    finding_decisions: Option<serde_json::Value>,
    // Some models put the report's identity beside it instead of inside it.
    contract_version: Option<serde_json::Value>,
    snapshot_id: Option<serde_json::Value>,
}

/// Escapes backslashes the model left unescaped inside JSON strings, as in a quoted `\d`.
/// Valid escapes are kept, so quotes still have to match the frozen source exactly.
fn escape_stray_backslashes(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let (mut in_string, mut chars) = (false, text.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_string = !in_string;
                output.push(c);
            }
            '\\' if in_string => match chars.peek() {
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                    output.push(c);
                    output.push(chars.next().unwrap());
                }
                _ => output.push_str("\\\\"),
            },
            _ => output.push(c),
        }
    }
    output
}

/// Drops closing brackets that close nothing open, as in a finding the model ended with `}}`.
/// Only structure changes: every string keeps its exact bytes.
fn drop_unmatched_closers(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut open = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else {
            match c {
                '"' => in_string = true,
                '{' | '[' => open.push(c),
                '}' | ']' => {
                    if open.last() != Some(&if c == '}' { '{' } else { '[' }) {
                        continue;
                    }
                    open.pop();
                }
                _ => {}
            }
        }
        output.push(c);
    }
    output
}

/// Where an object opening with the `report` key starts, for a sentence the model ran straight
/// into the object.
fn report_object(text: &str) -> Option<usize> {
    text.match_indices('{')
        .map(|(index, _)| index)
        .find(|&index| text[index + 1..].trim_start().starts_with("\"report\""))
}

fn final_response(transcript: Option<&str>) -> Result<(String, Option<String>), String> {
    let raw = transcript.ok_or("Review final response is missing: no transcript")?;
    parse_final_response(&crate::providers::claude::final_result(raw)?, false)
}

/// `lenient` is for the CLIs other than Claude, which sometimes send an object as a JSON string,
/// the decisions list bare, or an empty list when no decisions were asked for. Claude's output
/// stays strict.
fn parse_final_response(text: &str, lenient: bool) -> Result<(String, Option<String>), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Review final response is empty".into());
    }
    // Only the final answer counts, never earlier steps or tool output. Claude may wrap the
    // object in a fence or put a short sentence before it, on its own line or right before the
    // object; exactly one object is read. A fence counts only before the object: a finding's text
    // can itself mention one.
    let object = text.find("\n{").map(|line| line + 1).or_else(|| report_object(text));
    let fence = text
        .find("```")
        .filter(|&open| !text.starts_with('{') && object.is_none_or(|object| open < object));
    let start = match fence {
        _ if text.starts_with('{') => 0,
        Some(open) => {
            open + text[open..]
                .find('\n')
                .ok_or("Review final response has an unclosed JSON fence")?
                + 1
        }
        None => object.ok_or_else(|| {
            format!(
                "Review final response has no JSON object ({} characters of text)",
                text.chars().count()
            )
        })?,
    };
    let parse = |body: &str| {
        let mut objects = serde_json::Deserializer::from_str(body).into_iter::<FinalResponse>();
        objects
            .next()
            .ok_or_else(|| "Review final response is empty".to_string())?
            .map(|response| (response, body[objects.byte_offset()..].trim_start().starts_with("```")))
            .map_err(|error| format!("Review final response is not valid structured JSON ({error})"))
    };
    let (response, closed) = parse(&text[start..]).or_else(|error| {
        let escaped = escape_stray_backslashes(&text[start..]);
        parse(&escaped)
            .or_else(|_| parse(&drop_unmatched_closers(&escaped)))
            .map_err(|_| error)
    })?;
    if fence.is_some() && !text.starts_with('{') && !closed {
        return Err("Review final response has an unclosed JSON fence".into());
    }
    let unwrap = |value: serde_json::Value| match value {
        serde_json::Value::String(raw) if lenient => {
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw))
        }
        value => value,
    };
    let mut report = unwrap(response.report);
    for (key, value) in [
        ("contractVersion", response.contract_version),
        ("snapshotId", response.snapshot_id),
    ] {
        let (Some(value), Some(fields)) = (value, report.as_object_mut()) else {
            continue;
        };
        // The report's own value wins; validation checks it against the stage either way.
        fields.entry(key).or_insert(value);
    }
    let decisions = response.finding_decisions.map(unwrap);
    let decisions = if lenient {
        decisions
            .filter(|value| !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty()))
            .map(|value| match value {
                // A bare list takes the frozen identity from the report, as the wrapper would carry it.
                serde_json::Value::Array(items) => {
                    let mut wrapped = serde_json::json!({ "decisions": items });
                    for key in ["contractVersion", "snapshotId"] {
                        if let Some(value) = report.get(key) {
                            wrapped[key] = value.clone();
                        }
                    }
                    wrapped
                }
                value => value,
            })
    } else {
        decisions
    };
    let encode = |value: serde_json::Value| -> Result<String, String> {
        if !value.is_object() {
            return Err("Review final response requires JSON objects for report and findingDecisions".into());
        }
        let raw = value.to_string();
        if raw.len() > 2_000_000 {
            return Err("Review final response exceeds its output byte budget".into());
        }
        Ok(raw)
    };
    Ok((encode(report)?, decisions.map(encode).transpose()?))
}

#[cfg(unix)]
fn run(
    stream: &mut UnixStream,
    request: Request,
    archive: Option<crate::review_archive::Archive>,
) -> Result<(), String> {
    validate(&request)?;
    let version = cli_version()?;
    if matches!(request, Request::Catalog {}) {
        return send(
            stream,
            &Response::Catalog {
                models: vec![crate::providers::claude::MODEL.into()],
                version,
            },
        );
    }
    let Request::Run {
        model,
        version: expected,
        prompt,
        files,
        archive_id,
        preflight,
        account,
        provider,
        effort,
    } = request
    else {
        return Err("Invalid runner operation".into());
    };
    // Only models OpenCodeReview offers for the provider reach a CLI.
    let (model, effort) = crate::providers::engines::resolve(&provider, &model, &effort)?;
    let archive = archive.ok_or("Frozen archive must be installed before a stage")?;
    if archive_id.as_deref() != Some(archive.descriptor().id.as_str()) {
        return Err("Stage archive identity mismatch".into());
    }
    archive.validate_stage_files(&files).map_err(|e| e.to_string())?;
    if expected != version {
        return Err("CLI version changed; prepare an experiment with aligned runners".into());
    }
    archive
        .materialize_new(Path::new("/evidence/snapshot"), &AtomicBool::new(false))
        .map_err(|e| e.to_string())?;
    for (name, content) in files {
        let path = Path::new("/work").join(name);
        private_dir(path.parent().ok_or("Missing evidence parent")?, 0o755)?;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o444)
            .open(path)
            .and_then(|mut f| {
                f.write_all(content.as_bytes())?;
                f.set_permissions(fs::Permissions::from_mode(0o444))
            })
            .map_err(|e| e.to_string())?;
    }
    fs::write("/work/prompt.md", prompt).map_err(|e| e.to_string())?;
    fs::set_permissions("/work/prompt.md", fs::Permissions::from_mode(0o444)).map_err(|e| e.to_string())?;
    let mut probe = Command::new("/bin/sh");
    // The model UID must not reach any account's sign-in: Codex runs shell commands freely.
    let auth_root = std::env::var("OCR_RUNNER_AUTH_ROOT").unwrap_or_else(|_| "/claude-auth/app-managed".into());
    probe.args(["-c", "test -r /work/context/PR.md && test -r /evidence/snapshot/manifest.json && ! test -w /evidence/snapshot && ! test -w /work/context && ! test -x /ipc && ! test -r \"$1\" && ! test -x \"$1\"", "isolation-probe"]).arg(&auth_root).uid(UID).gid(UID);
    bounded(&mut probe, Path::new("/scratch"), &AtomicBool::new(false), 10)
        .map_err(|_| "Isolation runtime permissions failed preflight")?;
    let primary = archive
        .entries()
        .iter()
        .find(|entry| entry.exclusion.is_none())
        .map(|entry| {
            Path::new("/evidence/snapshot/trees")
                .join(&entry.source.revision)
                .join(&entry.source.path)
        });
    let dependencies = archive.dependencies().iter().filter_map(|dependency| {
        dependency
            .entries
            .iter()
            .find(|entry| entry.exclusion.is_none())
            .map(|entry| {
                Path::new("/evidence/snapshot/dependencies")
                    .join(&dependency.repository)
                    .join(&dependency.revision)
                    .join(&entry.source.path)
            })
    });
    for path in primary.into_iter().chain(dependencies) {
        let mut read = Command::new("/bin/sh");
        read.args(["-c", "test -r \"$1\" && ! test -w \"$1\"", "snapshot-probe"])
            .arg(path)
            .uid(UID)
            .gid(UID);
        bounded(&mut read, Path::new("/scratch"), &AtomicBool::new(false), 10)
            .map_err(|_| "Frozen source is not readable and immutable for model UID")?;
    }
    let (mut command, signed_in) = match provider.as_str() {
        "codex" => {
            let (command, auth) = codex_session(account, &model, &effort)?;
            (command, Some(auth))
        }
        "opencode" => {
            let (command, auth) = opencode_session(account)?;
            (command, Some(auth))
        }
        _ => {
            let mut command = session(account)?;
            inference(&mut command, &model, &effort)?;
            (command, None)
        }
    };
    if preflight {
        return send(stream, &Response::Ready);
    }
    let stdout = fs::File::create(TRANSCRIPT).map_err(|e| e.to_string())?;
    let stderr = fs::File::create("/scratch/stderr").map_err(|e| e.to_string())?;
    command.stdin(fs::File::open("/work/prompt.md").map_err(|e| e.to_string())?);
    command.stdout(stdout).stderr(stderr);
    stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .map_err(|e| e.to_string())?;
    check_control(stream)?;
    let mut child = command
        .spawn()
        .map_err(|_| "Isolation runtime unavailable: cannot start the unprivileged model CLI")?;
    let started = Instant::now();
    let outcome = (|| {
        send(stream, &Response::Started)?;
        loop {
            check_control(stream)?;
            if started.elapsed() > Duration::from_secs(900) {
                return Err("Stage inference timed out".into());
            }
            for (name, limit) in [(TRANSCRIPT, 32_000_000), ("/scratch/stderr", 8_000_000)] {
                if fs::symlink_metadata(name).is_ok_and(|m| !m.is_file() || m.len() > limit) {
                    return Err("Stage output limit exceeded".into());
                }
            }
            match child.try_wait() {
                // OpenCode reports a provider error as an event and still exits 0.
                Ok(Some(status))
                    if status.success()
                        && provider == "opencode"
                        && !crate::providers::opencode::errors(
                            &read_output(Path::new(TRANSCRIPT), 32_000_000)?.unwrap_or_default(),
                        )
                        .is_empty() =>
                {
                    let stderr = read_output(Path::new("/scratch/stderr"), 8_000_000)?.unwrap_or_default();
                    return Err(crate::providers::opencode::failure(
                        &read_output(Path::new(TRANSCRIPT), 32_000_000)?.unwrap_or_default(),
                        &stderr,
                    ));
                }
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    let stderr = read_output(Path::new("/scratch/stderr"), 8_000_000)?.unwrap_or_default();
                    let stdout = read_output(Path::new(TRANSCRIPT), 32_000_000)?.unwrap_or_default();
                    return Err(match provider.as_str() {
                        "codex" => crate::providers::codex::failure(&stdout, &stderr),
                        "opencode" => crate::providers::opencode::failure(&stdout, &stderr),
                        _ => crate::providers::claude::failure(&stdout, &stderr),
                    });
                }
                Ok(None) => {
                    send(stream, &Response::Pulse)?;
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(_) => return Err("Runner child supervision failed".into()),
            }
        }
    })();
    terminate(&mut child)?;
    let mut transcript = read_output(Path::new(TRANSCRIPT), 32_000_000)?;
    let Some(auth) = signed_in else {
        let result = outcome.and_then(|()| final_response(transcript.as_deref()));
        let (report, decisions, error) = match result {
            Ok((report, decisions)) => (Some(report), decisions, None),
            Err(error) => (None, None, Some(error)),
        };
        return send(
            stream,
            &Response::Finished {
                report,
                decisions,
                transcript,
                error,
                credential: None,
            },
        );
    };
    // Codex may refresh their sign-in mid-stage; the broker keeps the new one so the
    // next stage doesn't start from a revoked token. OpenCode's key never changes.
    let (credential_path, limit, validate, secrets_of): (
        Option<PathBuf>,
        usize,
        fn(&str) -> Result<(), String>,
        fn(&str) -> Vec<String>,
    ) = match provider.as_str() {
        "opencode" => (
            None,
            crate::providers::opencode::MAX_AUTH_BYTES,
            crate::providers::opencode::validate_auth,
            crate::providers::opencode::secrets,
        ),
        _ => (
            Some(Path::new(CODEX_HOME).join("auth.json")),
            crate::providers::codex::MAX_AUTH_BYTES,
            crate::providers::codex::validate_auth,
            crate::providers::codex::secrets,
        ),
    };
    let refreshed = credential_path
        .and_then(|path| read_output(&path, limit as u64).ok().flatten())
        .filter(|raw| *raw != auth && validate(raw).is_ok());
    let secrets = secrets_of(&auth)
        .into_iter()
        .chain(refreshed.as_deref().map(secrets_of).unwrap_or_default())
        .collect::<Vec<_>>();
    let result = outcome
        .and_then(|()| {
            let text = match provider.as_str() {
                "opencode" => crate::providers::opencode::final_message(transcript.as_deref().unwrap_or_default()),
                _ => read_output(&Path::new(CODEX_HOME).join("last-message.txt"), 4_000_000)?
                    .ok_or("Codex final response is missing")?,
            };
            parse_final_response(&text, true)
        })
        .and_then(|(report, decisions)| {
            if secrets.iter().any(|secret| {
                report.contains(secret) || decisions.as_ref().is_some_and(|decisions| decisions.contains(secret))
            }) {
                return Err("Review output contained the account's sign-in; discarded".into());
            }
            Ok((report, decisions))
        });
    if let Some(raw) = transcript.as_mut() {
        for secret in &secrets {
            *raw = raw.replace(secret.as_str(), "[redacted]");
        }
    }
    let (report, decisions, error) = match result {
        Ok((report, decisions)) => (Some(report), decisions, None),
        Err(error) => (None, None, Some(error)),
    };
    send(
        stream,
        &Response::Finished {
            report,
            decisions,
            transcript,
            error,
            credential: refreshed,
        },
    )
}

#[cfg(unix)]
pub(crate) fn check_control(stream: &mut UnixStream) -> Result<(), String> {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Ok(1) if byte == [1] => Err("cancelled".into()),
        Ok(1) => Err("Invalid runner control message".into()),
        _ => Err("Broker disconnected; inference cancelled".into()),
    }
}

#[cfg(unix)]
pub(crate) fn terminate(child: &mut std::process::Child) -> Result<(), String> {
    let _ = Command::new("/bin/bash")
        .args([
            "-c",
            "kill -KILL -- \"-$1\"",
            "sandbox-terminate",
            &child.id().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..10 {
        if ensure_no_model_processes().is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    ensure_no_model_processes()
}

#[cfg(unix)]
pub(crate) fn ensure_no_model_processes() -> Result<(), String> {
    for entry in fs::read_dir("/proc").map_err(|_| "Runner process isolation unavailable")? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let status = match fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot verify runner process cleanup".into()),
        };
        let model = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .is_some_and(|uids| uids.split_whitespace().any(|uid| uid.parse::<u32>() == Ok(UID)));
        let zombie = status
            .lines()
            .find_map(|line| line.strip_prefix("State:"))
            .is_some_and(|state| state.trim().starts_with('Z'));
        if model && !zombie {
            return Err("Runner descendant survived cleanup; stopping container before accepting another stage".into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn receive_job(stream: &mut UnixStream) -> Result<(Request, Option<crate::review_archive::Archive>), String> {
    use base64::Engine;
    let first = receive(stream)?;
    let Request::Install {
        descriptor,
        manifest,
        engine_version,
    } = first
    else {
        return Ok((first, None));
    };
    if engine_version != crate::sandbox::engine_version() {
        return Err("Isolation runtime executor version differs".into());
    }
    let mut incoming =
        crate::review_archive::Incoming::new(descriptor, manifest.into_bytes()).map_err(|e| e.to_string())?;
    let started = Instant::now();
    send(stream, &Response::Ack)?;
    loop {
        if started.elapsed() > Duration::from_secs(300) {
            return Err("Archive installation timed out".into());
        }
        match receive(stream)? {
            Request::Blob { hash, data } => {
                if data.len() > 1_333_336 {
                    return Err("Archive blob frame exceeds budget".into());
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| "Invalid blob encoding")?;
                incoming.push(&hash, &bytes).map_err(|e| e.to_string())?;
                send(stream, &Response::Ack)?;
            }
            Request::Seal {} => {
                let archive = incoming.finish().map_err(|e| e.to_string())?;
                send(stream, &Response::Ack)?;
                return Ok((receive(stream)?, Some(archive)));
            }
            _ => return Err("Unexpected archive installation operation".into()),
        }
    }
}

#[cfg(unix)]
pub fn serve() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("Runner requires the dedicated Docker Linux runtime".into());
    }
    private_dir(Path::new("/ipc"), 0o700)?;
    // Replicas share the IPC directory; each listens on a socket named after its container.
    let host = fs::read_to_string("/etc/hostname").map_err(|e| e.to_string())?;
    let host = host.trim();
    if host.is_empty() || host.len() > 64 || !host.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') {
        return Err("Unexpected runner hostname".into());
    }
    let socket = PathBuf::from(format!("/ipc/{host}.sock"));
    if let Ok(metadata) = fs::symlink_metadata(&socket) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if !metadata.file_type().is_socket() || metadata.uid() != 0 {
            return Err("Unexpected runner IPC entry".into());
        }
        match UnixStream::connect(&socket) {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                fs::remove_file(&socket).map_err(|e| e.to_string())?
            }
            _ => return Err("Runner socket is already active or inaccessible".into()),
        }
    }
    let kept = kept_work_entries(std::env::var("OCR_RUNNER_KEEP").ok().as_deref())?;
    let listener = UnixListener::bind(&socket).map_err(|e| e.to_string())?;
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(10))))
            .map_err(|e| e.to_string())?;
        private_dir(Path::new("/output"), 0o777)?;
        let result = receive_job(&mut stream).and_then(|(request, archive)| run(&mut stream, request, archive));
        if let Err(error) = result {
            let _ = send(&mut stream, &Response::Error { error });
        }
        ensure_no_model_processes()?;
        for directory in ["/work", "/output", "/scratch", "/tmp", "/evidence"] {
            for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if directory == "/work" && kept.iter().any(|name| path == Path::new("/work").join(name)) {
                    continue;
                }
                if fs::symlink_metadata(&path).map_err(|e| e.to_string())?.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                }
                .map_err(|_| "Runner cleanup failed; no more work admitted")?;
            }
        }
    }
    Ok(())
}

/// Entries directly under `/work` that the runner leaves in place between jobs: the benchmark
/// image (`ocr-bench`) mounts its inputs there. Stage files never use these names. Unset in the
/// product, where `/work` is the runner's own tmpfs.
pub(crate) fn kept_work_entries(raw: Option<&str>) -> Result<Vec<String>, String> {
    let names = raw
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for name in &names {
        if name.len() > 32
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || matches!(name.as_str(), "context" | "integration" | "prompt.md")
        {
            return Err("Invalid OCR_RUNNER_KEEP entry".into());
        }
    }
    Ok(names)
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

pub(crate) fn bounded(
    command: &mut Command,
    directory: &Path,
    cancel: &AtomicBool,
    seconds: u64,
) -> Result<Vec<u8>, String> {
    let output = directory.join(format!("command-{}.out", secret()?));
    let file = fs::File::create(&output).map_err(|e| e.to_string())?;
    command.stdout(file).stderr(Stdio::null()).stdin(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    let result = loop {
        if cancel.load(Ordering::Relaxed)
            || start.elapsed() > Duration::from_secs(seconds)
            || fs::metadata(&output).is_ok_and(|m| m.len() > 2_000_000)
        {
            crate::review::kill_tree(child.id(), &mut child);
            break Err("Command cancelled, oversized or timed out".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    fs::read(&output).map_err(|e| e.to_string())
                } else {
                    Err("Unexpected command status; verify runtime compatibility".into())
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                crate::review::kill_tree(child.id(), &mut child);
                break Err(error.to_string());
            }
        }
    };
    #[cfg(target_os = "linux")]
    let cleanup = terminate(&mut child);
    #[cfg(not(target_os = "linux"))]
    let cleanup = {
        crate::review::kill_tree(child.id(), &mut child);
        Ok::<(), String>(())
    };
    let _ = fs::remove_file(output);
    cleanup?;
    result
}
