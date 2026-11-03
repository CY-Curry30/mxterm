//! A standard WinForms AxHost owns the OLE frame and message loop. Rust owns
//! credentials, the private inherited pipes and the child process lifetime.
use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use tauri::{Emitter, Manager};

const BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mxterm-rdp.exe"));
const TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Serialize)]
pub(super) struct Config {
    title: String,
    host: String,
    port: u16,
    username: String,
    domain: Option<String>,
    password: Option<String>,
    width: u32,
    height: u32,
    keyboard_hook_mode: i32,
    audio_mode: i32,
    authentication_level: i32,
    gateway_mode: i32,
    gateway_host: Option<String>,
    color_depth: i32,
    performance_flags: u32,
    dynamic_resize: bool,
    use_multimon: bool,
    clipboard: bool,
    drives: bool,
    printers: bool,
    smart_cards: bool,
    nla: bool,
    appearance: Option<NativeRdpAppearance>,
}

impl Config {
    pub(super) fn new(
        resolved: &ResolvedRdpConnection,
        bounds: Option<&RdpEmbeddedBounds>,
        appearance: Option<NativeRdpAppearance>,
    ) -> Self {
        let rdp = &resolved.rdp;
        let experience = rdp_experience_settings(&rdp.performance);
        Self {
            title: format!("MXterm RDP - {}", resolved.profile.name),
            host: resolved.profile.host.clone(),
            port: resolved.profile.port,
            username: resolved.profile.username.clone(),
            domain: rdp.domain.clone(),
            password: resolved.password.clone(),
            width: rdp
                .display
                .width
                .map(u32::from)
                .unwrap_or_else(|| bounds.map_or(1440, |b| b.width)),
            height: rdp
                .display
                .height
                .map(u32::from)
                .unwrap_or_else(|| bounds.map_or(900, |b| b.height)),
            keyboard_hook_mode: rdp.resources.keyboard_hook_mode.as_rdp_value(),
            audio_mode: match rdp.resources.audio {
                RdpAudioMode::Local => 0,
                RdpAudioMode::Remote => 1,
                RdpAudioMode::Disabled => 2,
            },
            authentication_level: match rdp.security.certificate_policy {
                RdpCertificatePolicy::Trust => 0,
                RdpCertificatePolicy::Strict => 1,
                RdpCertificatePolicy::Prompt => 2,
            },
            gateway_mode: match rdp.gateway.as_ref().map(|g| &g.mode) {
                Some(RdpGatewayMode::Explicit) => 1,
                Some(RdpGatewayMode::Auto) => 2,
                _ => 0,
            },
            gateway_host: rdp.gateway.as_ref().and_then(|g| g.host.clone()),
            color_depth: i32::from(experience.session_bpp),
            performance_flags: experience.performance_flags() as u32,
            dynamic_resize: rdp.display.dynamic_resize,
            use_multimon: rdp.display.use_multimon
                || matches!(rdp.display.mode, RdpDisplayMode::AllMonitors),
            clipboard: rdp.resources.clipboard,
            drives: rdp.resources.drives,
            printers: rdp.resources.printers,
            smart_cards: rdp.resources.smart_cards,
            nla: !matches!(rdp.security.nla, RdpNetworkLevelAuthentication::Disabled),
            appearance,
        }
    }
}

struct Pipe {
    input: ChildStdin,
    responses: mpsc::Receiver<Value>,
}

pub(super) struct Host {
    pub(super) pid: u32,
    pub(super) hwnd: isize,
    pub(super) executable: PathBuf,
    child: Arc<Mutex<Child>>,
    pipe: Mutex<Pipe>,
    alive: Arc<AtomicBool>,
    sequence: AtomicU64,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinFormsRdpHost")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

fn failure(code: &str) -> AppError {
    AppError::new(code, "RDP 标准宿主不可用。", code, true)
}

pub(super) fn executable(app: &AppHandle) -> Result<PathBuf, AppError> {
    let digest = Sha256::digest(BINARY)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let directory = app
        .path()
        .app_cache_dir()
        .map_err(|_| failure("rdp_host_cache_failed"))?
        .join("rdp-host")
        .join(digest);
    fs::create_dir_all(&directory).map_err(|_| failure("rdp_host_cache_failed"))?;
    let path = directory.join("mxterm-rdp.exe");
    if fs::read(&path).is_ok_and(|data| data == BINARY) {
        return Ok(path);
    }
    let temporary = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, BINARY).map_err(|_| failure("rdp_host_extract_failed"))?;
    if fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&temporary);
        if !fs::read(&path).is_ok_and(|data| data == BINARY) {
            return Err(failure("rdp_host_extract_failed"));
        }
    }
    Ok(path)
}

fn state_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|p| p.join("rdp-host-window-state.json"))
}

fn emit_closed(app: &AppHandle, session_id: &str) {
    let _ = app.emit("rdp:session_closed", json!({ "session_id": session_id }));
}

// The reader never takes native_host: launch holds that lock to serialize
// process creation. Register before replying so an early close cannot resurrect
// a dead session after the caller receives the add acknowledgement.
fn event(app: &AppHandle, pid: u32, value: &Value) {
    let manager = app.state::<RdpSessionManager>();
    match value["event"].as_str() {
        Some("added") if value["ok"] == true => {
            if let (Some(id), Some(hwnd), Some(control)) = (
                value["session_id"].as_str(),
                value["hwnd"].as_i64(),
                value["control_hwnd"].as_i64(),
            ) {
                let _ = manager.insert(
                    id.into(),
                    ManagedRdpSession {
                        hwnd: hwnd as isize,
                        session_hwnd: Some(control as isize),
                        parent_hwnd: None,
                        process_id: Some(pid),
                        cleanup_path: None,
                        embedded: true,
                    },
                );
            }
        }
        Some("closed") => {
            if let Some(id) = value["session_id"].as_str() {
                if let Ok(mut sessions) = manager.sessions.lock() {
                    if sessions.get(id).is_some_and(|s| s.process_id == Some(pid)) {
                        sessions.remove(id);
                        emit_closed(app, id);
                    }
                }
            }
        }
        Some("connection") => {
            #[cfg(test)]
            if let Some(
                phase @ ("connecting" | "connected" | "login_complete" | "disconnected" | "fatal"
                | "reconnecting"),
            ) = value["phase"].as_str()
            {
                eprintln!(
                    "[rdp-test] phase={phase}, code={}",
                    value["code"].as_i64().unwrap_or(0)
                );
            }
        }
        Some("window_state") => {
            if let (Some(path), Ok(state)) = (
                state_path(app),
                serde_json::from_value::<PersistedRdpHostWindowState>(value["state"].clone()),
            ) {
                if state.width >= 200 && state.height >= 200 {
                    let temporary = path.with_extension("json.tmp");
                    if let Ok(data) = serde_json::to_vec(&state) {
                        if fs::write(&temporary, data).is_ok() {
                            let _ = fs::rename(&temporary, path);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn process_ended(app: &AppHandle, pid: u32) {
    let manager = app.state::<RdpSessionManager>();
    if let Ok(mut sessions) = manager.sessions.lock() {
        let ids: Vec<_> = sessions
            .iter()
            .filter(|(_, s)| s.process_id == Some(pid))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            sessions.remove(&id);
            emit_closed(app, &id);
        }
    };
}

impl Host {
    fn start(app: &AppHandle) -> Result<Arc<Self>, AppError> {
        let executable = executable(app)?;
        let mut child = Command::new(&executable)
            .arg("--stdio")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW, WinForms shows its own UI.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| failure("rdp_host_spawn_failed"))?;
        let pid = child.id();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, responses) = mpsc::channel();
        let reader_child = child.clone();
        let reader_alive = alive.clone();
        let reader_app = app.clone();
        if std::thread::Builder::new()
            .name("mxterm-rdp-ipc".into())
            .spawn(move || {
                for line in BufReader::new(output).lines() {
                    let Ok(line) = line else { break };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        break;
                    };
                    event(&reader_app, pid, &value);
                    if value["event"] == "ready" || value["id"].as_u64().is_some_and(|id| id > 0) {
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                }
                reader_alive.store(false, Ordering::Release);
                if let Ok(mut child) = reader_child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                process_ended(&reader_app, pid);
            })
            .is_err()
        {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(failure("rdp_host_reader_failed"));
        }
        let ready = responses.recv_timeout(TIMEOUT);
        let hwnd = ready
            .as_ref()
            .ok()
            .filter(|v| v["event"] == "ready" && v["version"] == 1)
            .and_then(|v| v["hwnd"].as_i64());
        let Some(hwnd) = hwnd else {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
            return Err(failure("rdp_host_handshake_failed"));
        };
        Ok(Arc::new(Self {
            pid,
            hwnd: hwnd as isize,
            executable,
            child,
            alive,
            pipe: Mutex::new(Pipe { input, responses }),
            sequence: AtomicU64::new(1),
        }))
    }

    pub(super) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(super) fn terminate(&self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }

    fn request(&self, mut request: Value) -> Result<Value, AppError> {
        let mut pipe = self
            .pipe
            .lock()
            .map_err(|_| failure("rdp_host_pipe_failed"))?;
        if !self.is_alive() {
            return Err(failure("rdp_host_exited"));
        }
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        request["id"] = json!(id);
        let result = (|| {
            serde_json::to_writer(&mut pipe.input, &request)
                .map_err(|_| failure("rdp_host_write_failed"))?;
            // Drop the request's credential copies immediately after serialization.
            request = Value::Null;
            pipe.input
                .write_all(b"\n")
                .and_then(|_| pipe.input.flush())
                .map_err(|_| failure("rdp_host_write_failed"))?;
            let value = pipe
                .responses
                .recv_timeout(TIMEOUT)
                .map_err(|_| failure("rdp_host_response_timeout"))?;
            if value["id"].as_u64() != Some(id) {
                return Err(failure("rdp_host_protocol_failed"));
            }
            Ok(value)
        })();
        // A timed-out add could otherwise create an untracked remote session.
        if result.is_err() {
            self.terminate();
        }
        let value = result?;
        if value["exiting"] == true {
            self.alive.store(false, Ordering::Release);
        }
        if value["ok"] != true {
            return Err(AppError::new(
                "rdp_host_command_failed",
                "RDP 标准宿主执行失败。",
                format!("hresult={}", value["hresult"].as_i64().unwrap_or(0)),
                true,
            ));
        }
        Ok(value)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(super) async fn launch(
    app: &AppHandle,
    session_id: String,
    config: Config,
    bounds: Option<RdpEmbeddedBounds>,
) -> Result<(u32, PathBuf), AppError> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let manager = app.state::<RdpSessionManager>();
        let mut slot = manager.native_host.lock().map_err(|_| failure("rdp_session_lock_failed"))?;
        if slot.as_ref().is_none_or(|h| !h.is_alive()) { *slot = Some(Host::start(&app)?); }
        let host = slot.as_ref().unwrap();
        let state = state_path(&app).and_then(|p| fs::read(p).ok()).and_then(|v| serde_json::from_slice::<PersistedRdpHostWindowState>(&v).ok());
        let request = json!({ "command": "add", "session_id": session_id, "config": config, "bounds": bounds, "window_state": state });
        drop(config);
        host.request(request)?;
        if manager.get(&session_id)?.is_none() { return Err(failure("rdp_host_session_closed")); }
        Ok((host.pid, host.executable.clone()))
    }).await.map_err(|_| failure("rdp_host_task_failed"))?
}

pub(super) fn command(
    manager: &RdpSessionManager,
    session: &ManagedRdpSession,
    name: &str,
    id: &str,
    bounds: Option<&RdpEmbeddedBounds>,
) -> Result<(), AppError> {
    let host = manager
        .native_host
        .lock()
        .map_err(|_| failure("rdp_session_lock_failed"))?
        .as_ref()
        .filter(|h| Some(h.pid) == session.process_id && h.hwnd == session.hwnd && h.is_alive())
        .cloned()
        .ok_or_else(|| failure("rdp_host_exited"))?;
    host.request(json!({ "command": name, "session_id": id, "bounds": bounds }))?;
    Ok(())
}
