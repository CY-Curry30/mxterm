use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::app_error::AppError;
use crate::connections::{
    ConnectionProfile, ConnectionProtocol, RdpAudioMode, RdpCertificatePolicy, RdpConnectionConfig,
    RdpDisplayMode, RdpGatewayMode, RdpNetworkLevelAuthentication, RdpPerformanceConfig,
    RdpPerformancePreset, RdpRenderMode, RdpRunnerConfig, RdpRunnerKind,
};
use crate::storage_repository::StorageRepository;

#[cfg(windows)]
mod winforms;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeRdpAppearance {
    pub panel: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub primary: u32,
    pub danger: u32,
    #[serde(default)]
    pub chrome: Option<u32>,
    #[serde(default)]
    pub chrome_line: Option<u32>,
    #[serde(default)]
    pub active: Option<u32>,
    #[serde(default)]
    pub hover: Option<u32>,
    #[serde(default)]
    pub success: Option<u32>,
    pub reduced_motion: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RdpConnectionRequest {
    pub connection_id: String,
    #[serde(default)]
    pub bounds: Option<RdpEmbeddedBounds>,
    #[serde(default)]
    pub appearance: Option<NativeRdpAppearance>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RdpRunnerProbeRequest {
    #[serde(default)]
    pub config: Option<RdpRunnerConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RdpSessionRequest {
    pub session_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RdpEmbeddedBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RdpResizeRequest {
    pub session_id: String,
    pub bounds: RdpEmbeddedBounds,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RdpPlatform {
    Windows,
    Linux,
    Macos,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpRunnerProbeResult {
    pub platform: RdpPlatform,
    pub available_runners: Vec<RdpRunnerKind>,
    pub default_runner: Option<RdpRunnerKind>,
    pub default_executable: Option<String>,
    pub supports_embedded: bool,
    pub supports_remote_app: bool,
    pub supports_dynamic_resize: bool,
    pub setup_hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpLaunchPreview {
    pub connection_id: String,
    pub runner: Option<RdpRunnerKind>,
    pub render_mode: RdpRenderMode,
    pub executable: Option<String>,
    pub args: Vec<String>,
    pub rdp_file_content: Option<String>,
    pub fallback_reason: Option<String>,
    pub setup_hint: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpLaunchResult {
    pub session_id: String,
    pub connection_id: String,
    pub launched: bool,
    pub embedded: bool,
    pub runner: RdpRunnerKind,
    pub executable: Option<String>,
    pub args: Vec<String>,
    pub process_id: Option<u32>,
    pub rdp_file_path: Option<String>,
    pub fallback_reason: Option<String>,
    pub setup_hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpSessionCloseResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpSessionResizeResult {
    pub ok: bool,
    pub applied: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RdpSessionRevealResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedRdpHostWindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

#[derive(Debug, Default)]
pub struct RdpSessionManager {
    sessions: Mutex<std::collections::HashMap<String, ManagedRdpSession>>,
    #[cfg(windows)]
    native_host: Mutex<Option<std::sync::Arc<winforms::Host>>>,
}

#[derive(Debug)]
struct ManagedRdpSession {
    hwnd: isize,
    session_hwnd: Option<isize>,
    parent_hwnd: Option<isize>,
    process_id: Option<u32>,
    cleanup_path: Option<PathBuf>,
    embedded: bool,
}

impl RdpSessionManager {
    pub fn shutdown(&self) {
        #[cfg(windows)]
        if let Ok(mut host) = self.native_host.lock() {
            if let Some(host) = host.take() {
                host.terminate();
            }
        }
    }

    fn insert(&self, session_id: String, session: ManagedRdpSession) -> Result<(), AppError> {
        let mut sessions = self.sessions.lock().map_err(|error| {
            AppError::new(
                "rdp_session_lock_failed",
                "RDP 会话状态访问失败。",
                error.to_string(),
                true,
            )
        })?;
        sessions.insert(session_id, session);
        Ok(())
    }

    fn get(&self, session_id: &str) -> Result<Option<ManagedRdpSession>, AppError> {
        let sessions = self.sessions.lock().map_err(|error| {
            AppError::new(
                "rdp_session_lock_failed",
                "RDP 会话状态访问失败。",
                error.to_string(),
                true,
            )
        })?;
        Ok(sessions.get(session_id).cloned())
    }
}

impl Clone for ManagedRdpSession {
    fn clone(&self) -> Self {
        Self {
            hwnd: self.hwnd,
            session_hwnd: self.session_hwnd,
            parent_hwnd: self.parent_hwnd,
            process_id: self.process_id,
            cleanup_path: self.cleanup_path.clone(),
            embedded: self.embedded,
        }
    }
}

struct ResolvedRdpConnection {
    profile: ConnectionProfile,
    rdp: RdpConnectionConfig,
    password: Option<String>,
}

struct SelectedRunner {
    runner: RdpRunnerKind,
    executable: PathBuf,
    fallback_reason: Option<String>,
}

pub fn probe_runner(
    _app: &AppHandle,
    request: RdpRunnerProbeRequest,
) -> Result<RdpRunnerProbeResult, AppError> {
    let config = request.config.unwrap_or_default();
    let platform = current_platform();
    let mut available_runners = Vec::new();
    let mut default_runner = None;
    let mut default_executable = None;
    let mut setup_hint = None;

    if let Some(custom) = config
        .custom_executable
        .as_deref()
        .and_then(find_custom_executable)
    {
        available_runners.push(RdpRunnerKind::Custom);
        if matches!(config.render_mode, RdpRenderMode::Custom) {
            default_runner = Some(RdpRunnerKind::Custom);
            default_executable = Some(custom.to_string_lossy().to_string());
        }
    }

    match platform {
        RdpPlatform::Windows => {
            if let Some(_mstscax) = find_windows_mstscax() {
                available_runners.push(RdpRunnerKind::MstscActiveX);
                if default_runner.is_none() && matches!(config.render_mode, RdpRenderMode::Embedded)
                {
                    default_runner = Some(RdpRunnerKind::MstscActiveX);
                    #[cfg(windows)]
                    {
                        default_executable =
                            Some(winforms::executable(_app)?.to_string_lossy().to_string());
                    }
                    #[cfg(not(windows))]
                    {
                        default_executable = Some(_mstscax.to_string_lossy().to_string());
                    }
                }
            }
            if let Some(mstsc) = find_windows_mstsc() {
                available_runners.push(RdpRunnerKind::Mstsc);
                if default_runner.is_none() {
                    default_runner = Some(RdpRunnerKind::Mstsc);
                    default_executable = Some(mstsc.to_string_lossy().to_string());
                }
            } else {
                setup_hint = Some("未找到 mstsc.exe，请确认系统远程桌面客户端可用。".to_string());
            }
            if !available_runners.contains(&RdpRunnerKind::MstscActiveX) {
                setup_hint =
                    Some("未找到 mstscax.dll，嵌入式 RDP 将回退到外部 mstsc.exe。".to_string());
            }
        }
        RdpPlatform::Linux => {
            if let Some(freerdp) = find_first_executable(&["wlfreerdp", "xfreerdp"]) {
                available_runners.push(RdpRunnerKind::Freerdp);
                if default_runner.is_none() {
                    default_runner = Some(RdpRunnerKind::Freerdp);
                    default_executable = Some(freerdp.to_string_lossy().to_string());
                }
            } else {
                setup_hint =
                    Some("未找到 wlfreerdp 或 xfreerdp，请安装 FreeRDP 客户端。".to_string());
            }
        }
        RdpPlatform::Macos => {
            if let Ok(macos_app) = select_macos_app_runner() {
                available_runners.push(RdpRunnerKind::MacosApp);
                if default_runner.is_none() {
                    default_runner = Some(RdpRunnerKind::MacosApp);
                    default_executable = Some(macos_app.executable.to_string_lossy().to_string());
                    setup_hint = Some(match find_macos_rdp_app_name() {
                        Some(app_name) => {
                            format!("将使用 {app_name} 打开 .rdp 文件，凭据由客户端提示。")
                        }
                        None => "未检测到 Windows App / Microsoft Remote Desktop，将尝试系统默认 .rdp 处理程序；建议先安装官方 RDP 客户端。".to_string(),
                    });
                }
            } else if default_runner.is_none() {
                setup_hint = Some("未找到 macOS open 命令，无法启动系统 RDP 客户端。".to_string());
            }
        }
        RdpPlatform::Unknown => {
            setup_hint = Some("当前平台的 RDP 客户端适配将在后续按平台单独启用。".to_string());
        }
    }

    let supports_embedded = matches!(platform, RdpPlatform::Windows)
        && available_runners.contains(&RdpRunnerKind::MstscActiveX);

    Ok(RdpRunnerProbeResult {
        platform,
        available_runners,
        default_runner,
        default_executable,
        supports_embedded,
        supports_remote_app: true,
        supports_dynamic_resize: true,
        setup_hint,
    })
}

pub fn preview_launch(
    app: &AppHandle,
    request: RdpConnectionRequest,
) -> Result<RdpLaunchPreview, AppError> {
    let resolved = resolve_rdp_connection(app, &request.connection_id)?;
    let mut selected = select_runner(&resolved.rdp)?;
    if matches!(selected.runner, RdpRunnerKind::MstscActiveX) {
        if let Some(reason) = embedded_fallback_reason(&resolved.rdp, request.bounds.as_ref()) {
            selected = select_windows_mstsc_runner()?;
            selected.fallback_reason = Some(reason);
        } else {
            #[cfg(windows)]
            {
                selected.executable = winforms::executable(app)?;
            }
        }
    }
    let plan = build_launch_plan(app, &resolved, &selected, true)?;
    Ok(RdpLaunchPreview {
        connection_id: resolved.profile.id,
        runner: Some(selected.runner),
        render_mode: resolved.rdp.runner.render_mode,
        executable: Some(selected.executable.to_string_lossy().to_string()),
        args: plan.args,
        rdp_file_content: plan.rdp_file_content,
        fallback_reason: selected.fallback_reason,
        setup_hint: None,
        warnings: plan.warnings,
    })
}

pub async fn launch_connection(
    app: &AppHandle,
    _manager: &RdpSessionManager,
    request: RdpConnectionRequest,
) -> Result<RdpLaunchResult, AppError> {
    let resolved = resolve_rdp_connection(app, &request.connection_id)?;
    let requested_bounds = request.bounds.clone();
    let selected = select_runner(&resolved.rdp)?;

    if matches!(selected.runner, RdpRunnerKind::MstscActiveX) {
        if let Some(reason) = embedded_fallback_reason(&resolved.rdp, requested_bounds.as_ref()) {
            let fallback = select_windows_mstsc_runner()?;
            return launch_external_runner(app, &resolved, &fallback, Some(reason));
        }

        let session_id = format!("rdp-{}", uuid::Uuid::new_v4());
        match launch_native_host(
            app,
            session_id.clone(),
            &resolved,
            requested_bounds,
            request.appearance,
        )
        .await
        {
            Ok((process_id, executable)) => {
                return Ok(RdpLaunchResult {
                    session_id,
                    connection_id: resolved.profile.id,
                    launched: true,
                    embedded: false,
                    runner: RdpRunnerKind::MstscActiveX,
                    executable: Some(executable.to_string_lossy().to_string()),
                    args: Vec::new(),
                    process_id: Some(process_id),
                    rdp_file_path: None,
                    fallback_reason: Some(
                        "已打开 MXterm 原生 RDP 窗口，凭据通过私有内存管道交给标准 ActiveX 宿主。"
                            .into(),
                    ),
                    setup_hint: None,
                });
            }
            Err(error) => {
                let fallback = select_windows_mstsc_runner()?;
                return launch_external_runner(
                    app,
                    &resolved,
                    &fallback,
                    Some(format!(
                        "RDP ActiveX 原生子窗口初始化失败，已回退到外部 mstsc.exe：{}",
                        rdp_launch_error_summary(&error)
                    )),
                );
            }
        }
    }

    let fallback_reason = selected
        .fallback_reason
        .clone()
        .or_else(|| embedded_fallback_reason(&resolved.rdp, requested_bounds.as_ref()));

    launch_external_runner(app, &resolved, &selected, fallback_reason)
}

fn launch_external_runner(
    app: &AppHandle,
    resolved: &ResolvedRdpConnection,
    selected: &SelectedRunner,
    fallback_reason: Option<String>,
) -> Result<RdpLaunchResult, AppError> {
    let plan = build_launch_plan(app, resolved, selected, false)?;
    let mut command = Command::new(&selected.executable);
    command.args(&plan.args);
    let child = command.spawn().map_err(|error| {
        AppError::new(
            "rdp_launch_failed",
            "RDP 客户端启动失败。",
            format!(
                "executable={}, error={error}",
                selected.executable.display()
            ),
            true,
        )
    })?;
    let process_id = child.id();
    drop(child);

    if let Some(path) = plan.cleanup_path.clone() {
        schedule_temp_file_cleanup(path);
    }

    Ok(RdpLaunchResult {
        session_id: format!("rdp-{}", uuid::Uuid::new_v4()),
        connection_id: resolved.profile.id.clone(),
        launched: true,
        embedded: false,
        runner: selected.runner.clone(),
        executable: Some(selected.executable.to_string_lossy().to_string()),
        args: plan.args,
        process_id: Some(process_id),
        rdp_file_path: plan
            .rdp_file_path
            .map(|path| path.to_string_lossy().to_string()),
        fallback_reason,
        setup_hint: None,
    })
}

pub fn close_session(
    manager: &RdpSessionManager,
    request: RdpSessionRequest,
) -> RdpSessionCloseResult {
    let result = managed_host_command(manager, "close", &request.session_id, None);
    RdpSessionCloseResult {
        ok: result.is_ok(),
        message: result
            .map(|_| "RDP 会话已关闭。".into())
            .unwrap_or_else(|e| e.message),
    }
}

pub fn resize_embedded_session(
    manager: &RdpSessionManager,
    request: RdpResizeRequest,
) -> RdpSessionResizeResult {
    let result = managed_host_command(
        manager,
        "resize",
        &request.session_id,
        Some(&request.bounds),
    );
    RdpSessionResizeResult {
        ok: result.is_ok(),
        applied: result.is_ok(),
        message: result
            .map(|_| "已调整 RDP 宿主窗口。".into())
            .unwrap_or_else(|e| e.message),
    }
}

pub fn reveal_session(
    manager: &RdpSessionManager,
    request: RdpSessionRequest,
) -> RdpSessionRevealResult {
    let result = managed_host_command(manager, "activate", &request.session_id, None);
    RdpSessionRevealResult {
        ok: result.is_ok(),
        message: result
            .map(|_| "已唤起 RDP 会话。".into())
            .unwrap_or_else(|e| e.message),
    }
}

fn managed_host_command(
    manager: &RdpSessionManager,
    command: &str,
    id: &str,
    bounds: Option<&RdpEmbeddedBounds>,
) -> Result<(), AppError> {
    #[cfg(windows)]
    if let Some(session) = manager.get(id)?.filter(|s| s.embedded) {
        return winforms::command(manager, &session, command, id, bounds);
    }
    let _ = (manager, command, id, bounds);
    Err(AppError::new(
        "rdp_session_unavailable",
        "RDP 会话当前没有可用的内置宿主。",
        "unknown or external RDP session",
        true,
    ))
}

async fn launch_native_host(
    app: &AppHandle,
    session_id: String,
    resolved: &ResolvedRdpConnection,
    bounds: Option<RdpEmbeddedBounds>,
    appearance: Option<NativeRdpAppearance>,
) -> Result<(u32, PathBuf), AppError> {
    #[cfg(windows)]
    {
        return winforms::launch(
            app,
            session_id,
            winforms::Config::new(resolved, bounds.as_ref(), appearance),
            bounds,
        )
        .await;
    }
    #[cfg(not(windows))]
    {
        let _ = (app, session_id, resolved, bounds, appearance);
        Err(AppError::new(
            "rdp_embedded_unsupported",
            "当前平台不支持内置 RDP 宿主。",
            "Windows only",
            true,
        ))
    }
}

fn resolve_rdp_connection(
    app: &AppHandle,
    connection_id: &str,
) -> Result<ResolvedRdpConnection, AppError> {
    let connection_id = connection_id.trim();
    if connection_id.is_empty() {
        return Err(AppError::new(
            "rdp_connection_missing",
            "请选择 RDP 连接。",
            "connection_id is empty",
            true,
        ));
    }

    let repository = StorageRepository::open_app(app)?;
    let profile = repository.connection_get(connection_id)?.ok_or_else(|| {
        AppError::new(
            "connection_missing",
            "连接不存在。",
            format!("connection_id={connection_id}"),
            false,
        )
    })?;
    if profile.protocol != ConnectionProtocol::Rdp {
        return Err(AppError::new(
            "rdp_protocol_required",
            "该操作仅支持 RDP 连接。",
            format!(
                "connection_id={}, protocol={:?}",
                profile.id, profile.protocol
            ),
            true,
        ));
    }
    let rdp = profile.rdp.clone().unwrap_or_default();
    let password = repository.resolve_rdp_connection_secret(connection_id)?;
    Ok(ResolvedRdpConnection {
        profile,
        rdp,
        password,
    })
}

fn select_runner(rdp: &RdpConnectionConfig) -> Result<SelectedRunner, AppError> {
    if matches!(rdp.runner.render_mode, RdpRenderMode::Custom) {
        return select_custom_runner(&rdp.runner);
    }

    if matches!(rdp.runner.preferred_runner, Some(RdpRunnerKind::Custom)) {
        return select_custom_runner(&rdp.runner);
    }

    match current_platform() {
        RdpPlatform::Windows => {
            if matches!(rdp.runner.render_mode, RdpRenderMode::Embedded) {
                if let Some(executable) = find_windows_mstscax() {
                    return Ok(SelectedRunner {
                        runner: RdpRunnerKind::MstscActiveX,
                        executable,
                        fallback_reason: None,
                    });
                }
            }
            let executable = find_windows_mstsc().ok_or_else(|| {
                AppError::new(
                    "rdp_runner_missing",
                    "未找到 mstsc.exe。",
                    "mstsc.exe is missing",
                    true,
                )
            })?;
            Ok(SelectedRunner {
                runner: RdpRunnerKind::Mstsc,
                executable,
                fallback_reason: None,
            })
        }
        RdpPlatform::Linux => {
            let executable =
                find_first_executable(&["wlfreerdp", "xfreerdp"]).ok_or_else(|| {
                    AppError::new(
                        "rdp_runner_missing",
                        "未找到 FreeRDP 客户端。",
                        "wlfreerdp/xfreerdp missing",
                        true,
                    )
                })?;
            Ok(SelectedRunner {
                runner: RdpRunnerKind::Freerdp,
                executable,
                fallback_reason: None,
            })
        }
        RdpPlatform::Macos => select_macos_app_runner(),
        RdpPlatform::Unknown => select_custom_runner(&rdp.runner),
    }
}

fn select_custom_runner(runner: &RdpRunnerConfig) -> Result<SelectedRunner, AppError> {
    let executable = runner
        .custom_executable
        .as_deref()
        .and_then(find_custom_executable)
        .ok_or_else(|| {
            AppError::new(
                "rdp_custom_runner_missing",
                "请先配置自定义 RDP 客户端路径。",
                "custom executable is missing",
                true,
            )
        })?;
    Ok(SelectedRunner {
        runner: RdpRunnerKind::Custom,
        executable,
        fallback_reason: None,
    })
}

fn select_windows_mstsc_runner() -> Result<SelectedRunner, AppError> {
    let executable = find_windows_mstsc().ok_or_else(|| {
        AppError::new(
            "rdp_runner_missing",
            "未找到 mstsc.exe。",
            "mstsc.exe is missing",
            true,
        )
    })?;
    Ok(SelectedRunner {
        runner: RdpRunnerKind::Mstsc,
        executable,
        fallback_reason: None,
    })
}

fn select_macos_app_runner() -> Result<SelectedRunner, AppError> {
    let executable = PathBuf::from("/usr/bin/open");
    let executable = if executable.is_file() {
        executable
    } else {
        find_executable("open").ok_or_else(|| {
            AppError::new(
                "rdp_runner_missing",
                "未找到 macOS open 命令。",
                "open command is missing",
                true,
            )
        })?
    };

    Ok(SelectedRunner {
        runner: RdpRunnerKind::MacosApp,
        executable,
        fallback_reason: None,
    })
}

fn embedded_fallback_reason(
    rdp: &RdpConnectionConfig,
    _bounds: Option<&RdpEmbeddedBounds>,
) -> Option<String> {
    if !matches!(rdp.runner.render_mode, RdpRenderMode::Embedded) {
        return None;
    }
    if !matches!(current_platform(), RdpPlatform::Windows) {
        return Some("当前平台不支持 Windows RDP 嵌入式宿主，已使用外部 runner。".to_string());
    }
    if rdp.remote_app.enabled {
        return Some("RemoteApp 暂走 mstsc.exe 外部模式，避免嵌入宿主兼容性问题。".to_string());
    }
    None
}

fn rdp_launch_error_summary(error: &AppError) -> String {
    let raw = error.raw_message.trim();
    if raw.is_empty() || raw == error.message {
        return error.message.clone();
    }

    format!("{}；{}", error.message, truncate_rdp_detail(raw, 420))
}

fn truncate_rdp_detail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }

    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

struct LaunchPlan {
    args: Vec<String>,
    rdp_file_content: Option<String>,
    rdp_file_path: Option<PathBuf>,
    cleanup_path: Option<PathBuf>,
    warnings: Vec<String>,
}

fn build_launch_plan(
    app: &AppHandle,
    resolved: &ResolvedRdpConnection,
    selected: &SelectedRunner,
    preview: bool,
) -> Result<LaunchPlan, AppError> {
    match selected.runner {
        RdpRunnerKind::MstscActiveX => Ok(LaunchPlan {
            args: Vec::new(),
            rdp_file_content: None,
            rdp_file_path: None,
            cleanup_path: None,
            warnings: vec![
                "Windows 嵌入式模式使用 MSTSC ActiveX 原生宿主，不通过命令行传递密码。".to_string(),
            ],
        }),
        RdpRunnerKind::Mstsc => build_mstsc_plan(app, resolved, preview),
        RdpRunnerKind::Freerdp => build_freerdp_plan(resolved),
        RdpRunnerKind::MacosApp => build_macos_app_plan(app, resolved, preview),
        RdpRunnerKind::Custom => build_custom_plan(app, resolved, preview),
    }
}

fn build_mstsc_plan(
    app: &AppHandle,
    resolved: &ResolvedRdpConnection,
    preview: bool,
) -> Result<LaunchPlan, AppError> {
    let content = serialize_rdp_file(&resolved.profile, &resolved.rdp)?;
    if preview {
        return Ok(LaunchPlan {
            args: vec!["<temp.rdp>".to_string()],
            rdp_file_content: Some(content),
            rdp_file_path: None,
            cleanup_path: None,
            warnings: Vec::new(),
        });
    }
    let path = write_temp_rdp_file(app, &resolved.profile.id, &content)?;
    Ok(LaunchPlan {
        args: vec![path.to_string_lossy().to_string()],
        rdp_file_content: None,
        rdp_file_path: Some(path.clone()),
        cleanup_path: Some(path),
        warnings: Vec::new(),
    })
}

fn build_freerdp_plan(resolved: &ResolvedRdpConnection) -> Result<LaunchPlan, AppError> {
    let mut args = vec![
        format!("/v:{}:{}", resolved.profile.host, resolved.profile.port),
        format!("/u:{}", resolved.profile.username),
    ];
    if let Some(domain) = resolved.rdp.domain.as_deref() {
        args.push(format!("/d:{domain}"));
    }
    if resolved.rdp.display.dynamic_resize {
        args.push("/dynamic-resolution".to_string());
    } else if let (Some(width), Some(height)) =
        (resolved.rdp.display.width, resolved.rdp.display.height)
    {
        args.push(format!("/size:{}x{}", width, height));
    }
    if resolved.rdp.resources.clipboard {
        args.push("/clipboard".to_string());
    }
    if resolved.rdp.display.use_multimon {
        args.push("/multimon".to_string());
    }
    match resolved.rdp.resources.audio {
        RdpAudioMode::Local => args.push("/sound".to_string()),
        RdpAudioMode::Remote => args.push("/audio-mode:1".to_string()),
        RdpAudioMode::Disabled => args.push("/audio-mode:2".to_string()),
    }
    if resolved.rdp.resources.drives {
        args.push("/drive:home,~".to_string());
    }
    if let Some(raw) = resolved.rdp.raw_runner_args.as_deref() {
        args.extend(split_runner_args(raw)?);
    }

    Ok(LaunchPlan {
        args,
        rdp_file_content: None,
        rdp_file_path: None,
        cleanup_path: None,
        warnings: vec![
            "外部 FreeRDP runner 将自行提示凭据，MXterm 不会通过命令行传递密码。".to_string(),
        ],
    })
}

fn build_macos_app_plan(
    app: &AppHandle,
    resolved: &ResolvedRdpConnection,
    preview: bool,
) -> Result<LaunchPlan, AppError> {
    let mut plan = build_mstsc_plan(app, resolved, preview)?;
    if let Some(path_arg) = plan.args.first().cloned() {
        plan.args = macos_rdp_open_args(path_arg);
    }
    let client_label =
        find_macos_rdp_app_name().unwrap_or_else(|| "系统默认 RDP 客户端".to_string());
    plan.warnings.push(format!(
        "macOS 将通过 {client_label} 打开 .rdp 文件，MXterm 不会通过命令行传递密码。"
    ));
    Ok(plan)
}

fn build_custom_plan(
    app: &AppHandle,
    resolved: &ResolvedRdpConnection,
    preview: bool,
) -> Result<LaunchPlan, AppError> {
    let content = serialize_rdp_file(&resolved.profile, &resolved.rdp)?;
    let template = resolved
        .rdp
        .runner
        .custom_args_template
        .as_deref()
        .unwrap_or("{rdp_file}");
    let path = if preview {
        PathBuf::from("<temp.rdp>")
    } else {
        write_temp_rdp_file(app, &resolved.profile.id, &content)?
    };
    let rendered = template
        .replace("{rdp_file}", &path.to_string_lossy())
        .replace("{host}", &resolved.profile.host)
        .replace("{port}", &resolved.profile.port.to_string())
        .replace("{username}", &resolved.profile.username)
        .replace(
            "{domain}",
            resolved.rdp.domain.as_deref().unwrap_or_default(),
        );
    let args = split_runner_args(&rendered)?;
    Ok(LaunchPlan {
        args,
        rdp_file_content: preview.then_some(content),
        rdp_file_path: (!preview).then_some(path.clone()),
        cleanup_path: (!preview).then_some(path),
        warnings: vec!["自定义 runner 参数模板已禁用密码占位符，请让客户端提示凭据。".to_string()],
    })
}

fn serialize_rdp_file(
    profile: &ConnectionProfile,
    rdp: &RdpConnectionConfig,
) -> Result<String, AppError> {
    let mut lines = Vec::new();
    let experience = rdp_experience_settings(&rdp.performance);
    lines.push(format!("full address:s:{}:{}", profile.host, profile.port));
    lines.push(format!("username:s:{}", profile.username));
    if let Some(domain) = rdp.domain.as_deref() {
        lines.push(format!("domain:s:{domain}"));
    }
    lines.push(format!(
        "screen mode id:i:{}",
        if matches!(
            rdp.display.mode,
            RdpDisplayMode::Fullscreen | RdpDisplayMode::AllMonitors
        ) {
            2
        } else {
            1
        }
    ));
    if let Some(width) = rdp.display.width {
        lines.push(format!("desktopwidth:i:{width}"));
    }
    if let Some(height) = rdp.display.height {
        lines.push(format!("desktopheight:i:{height}"));
    }
    if rdp.display.dynamic_resize {
        // mstsc.exe 不会调用 ActiveX 的 UpdateSessionDisplaySettings；
        // smart sizing 让外部客户端至少将远端画面缩放到当前窗口内。
        lines.push("smart sizing:i:1".to_string());
    }
    lines.push(format!(
        "use multimon:i:{}",
        if rdp.display.use_multimon { 1 } else { 0 }
    ));
    // mstsc「本地资源 → 键盘 → 应用 Windows 组合键」三档：
    // 0=在这台计算机上、1=在远程计算机上、2=仅在全屏显示时。
    // 窗口模式下要转发 Win+E / Alt+Tab，必须写 1。
    lines.push(format!(
        "keyboardhook:i:{}",
        rdp.resources.keyboard_hook_mode.as_rdp_value()
    ));
    lines.push(format!("session bpp:i:{}", experience.session_bpp));
    lines.push(format!("connection type:i:{}", experience.connection_type));
    lines.push("networkautodetect:i:0".to_string());
    lines.push("bandwidthautodetect:i:0".to_string());
    lines.push(format!(
        "disable wallpaper:i:{}",
        bool_i32(experience.disable_wallpaper)
    ));
    lines.push(format!(
        "allow font smoothing:i:{}",
        bool_i32(experience.allow_font_smoothing)
    ));
    lines.push(format!(
        "allow desktop composition:i:{}",
        bool_i32(experience.allow_desktop_composition)
    ));
    lines.push(format!(
        "disable full window drag:i:{}",
        bool_i32(experience.disable_full_window_drag)
    ));
    lines.push(format!(
        "disable menu anims:i:{}",
        bool_i32(experience.disable_menu_anims)
    ));
    lines.push(format!(
        "disable themes:i:{}",
        bool_i32(experience.disable_themes)
    ));
    lines.push(format!(
        "disable cursor setting:i:{}",
        bool_i32(experience.disable_cursor_setting)
    ));
    lines.push("bitmapcachepersistenable:i:1".to_string());
    lines.push(format!(
        "redirectclipboard:i:{}",
        if rdp.resources.clipboard { 1 } else { 0 }
    ));
    lines.push(format!(
        "audiomode:i:{}",
        match rdp.resources.audio {
            RdpAudioMode::Local => 0,
            RdpAudioMode::Remote => 1,
            RdpAudioMode::Disabled => 2,
        }
    ));
    lines.push(format!(
        "redirectdrives:i:{}",
        if rdp.resources.drives { 1 } else { 0 }
    ));
    lines.push(format!(
        "redirectprinters:i:{}",
        if rdp.resources.printers { 1 } else { 0 }
    ));
    lines.push(format!(
        "redirectsmartcards:i:{}",
        if rdp.resources.smart_cards { 1 } else { 0 }
    ));
    lines.push(format!(
        "enablecredsspsupport:i:{}",
        match rdp.security.nla {
            RdpNetworkLevelAuthentication::Disabled => 0,
            RdpNetworkLevelAuthentication::Auto | RdpNetworkLevelAuthentication::Enabled => 1,
        }
    ));
    lines.push(format!(
        "authentication level:i:{}",
        match rdp.security.certificate_policy {
            RdpCertificatePolicy::Trust => 0,
            RdpCertificatePolicy::Strict => 1,
            RdpCertificatePolicy::Prompt => 2,
        }
    ));
    lines.push("prompt for credentials:i:1".to_string());
    if let Some(gateway) = rdp.gateway.as_ref() {
        if let Some(host) = gateway.host.as_deref() {
            lines.push(format!("gatewayhostname:s:{host}"));
        }
        lines.push(format!(
            "gatewayusagemethod:i:{}",
            match gateway.mode {
                RdpGatewayMode::Disabled => 0,
                RdpGatewayMode::Explicit => 1,
                RdpGatewayMode::Auto => 2,
            }
        ));
        lines.push("gatewaycredentialssource:i:4".to_string());
    }
    if rdp.remote_app.enabled {
        lines.push("remoteapplicationmode:i:1".to_string());
        if let Some(program) = rdp.remote_app.program.as_deref() {
            lines.push(format!("remoteapplicationprogram:s:{program}"));
        }
        if let Some(working_dir) = rdp.remote_app.working_dir.as_deref() {
            lines.push(format!("remoteapplicationcmdline:s:{working_dir}"));
        }
        if let Some(args) = rdp.remote_app.args.as_deref() {
            lines.push(format!("remoteapplicationexpandcmdline:s:{args}"));
        }
    }
    if let Some(raw) = rdp.raw_rdp_settings.as_deref() {
        lines.extend(
            raw.lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty()),
        );
    }
    Ok(format!("{}\r\n", lines.join("\r\n")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RdpExperienceSettings {
    session_bpp: u16,
    connection_type: u8,
    allow_font_smoothing: bool,
    allow_desktop_composition: bool,
    disable_full_window_drag: bool,
    disable_menu_anims: bool,
    disable_themes: bool,
    disable_wallpaper: bool,
    disable_cursor_setting: bool,
}

impl RdpExperienceSettings {
    #[cfg(windows)]
    fn performance_flags(self) -> i32 {
        let mut flags = 0i32;
        if self.disable_wallpaper {
            flags |= 0x0000_0001;
        }
        if self.disable_full_window_drag {
            flags |= 0x0000_0002;
        }
        if self.disable_menu_anims {
            flags |= 0x0000_0004;
        }
        if self.disable_themes {
            flags |= 0x0000_0008;
        }
        if self.disable_cursor_setting {
            flags |= 0x0000_0040;
        }
        if self.allow_font_smoothing {
            flags |= 0x0000_0080;
        }
        if self.allow_desktop_composition {
            flags |= 0x0000_0100;
        }
        flags
    }
}

fn rdp_experience_settings(performance: &RdpPerformanceConfig) -> RdpExperienceSettings {
    let allow_font_smoothing = performance.font_smoothing;
    let allow_desktop_composition = performance.visual_styles;
    let disable_themes = !performance.visual_styles;
    let disable_wallpaper = !performance.desktop_background;

    match performance.preset {
        RdpPerformancePreset::LowBandwidth => RdpExperienceSettings {
            session_bpp: 16,
            connection_type: 2,
            allow_font_smoothing,
            allow_desktop_composition: false,
            disable_full_window_drag: true,
            disable_menu_anims: true,
            disable_themes: true,
            disable_wallpaper: true,
            disable_cursor_setting: true,
        },
        RdpPerformancePreset::Balanced => RdpExperienceSettings {
            session_bpp: 24,
            connection_type: 3,
            allow_font_smoothing,
            allow_desktop_composition,
            disable_full_window_drag: true,
            disable_menu_anims: true,
            disable_themes,
            disable_wallpaper,
            disable_cursor_setting: true,
        },
        RdpPerformancePreset::Auto | RdpPerformancePreset::Lan => RdpExperienceSettings {
            session_bpp: 32,
            connection_type: 7,
            allow_font_smoothing,
            allow_desktop_composition,
            disable_full_window_drag: false,
            disable_menu_anims: false,
            disable_themes,
            disable_wallpaper,
            disable_cursor_setting: false,
        },
    }
}

fn bool_i32(value: bool) -> i32 {
    if value {
        1
    } else {
        0
    }
}

fn write_temp_rdp_file(
    app: &AppHandle,
    connection_id: &str,
    content: &str,
) -> Result<PathBuf, AppError> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| {
            AppError::new(
                "rdp_temp_path_failed",
                "RDP 临时目录获取失败。",
                error,
                true,
            )
        })?
        .join("rdp");
    fs::create_dir_all(&dir).map_err(|error| {
        AppError::new(
            "rdp_temp_write_failed",
            "RDP 临时目录创建失败。",
            error,
            true,
        )
    })?;
    let file_name = format!(
        "{}-{}.rdp",
        sanitize_file_stem(connection_id),
        uuid::Uuid::new_v4()
    );
    let path = dir.join(file_name);
    fs::write(&path, content).map_err(|error| {
        AppError::new(
            "rdp_temp_write_failed",
            "RDP 临时文件写入失败。",
            error,
            true,
        )
    })?;
    Ok(path)
}

fn sanitize_file_stem(value: &str) -> String {
    let stem = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if stem.is_empty() {
        "connection".to_string()
    } else {
        stem
    }
}

fn schedule_temp_file_cleanup(path: PathBuf) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let _ = tokio::fs::remove_file(path).await;
    });
}

fn split_runner_args(value: &str) -> Result<Vec<String>, AppError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    reject_secret_arg(&current)?;
                    args.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if quote.is_some() {
        return Err(AppError::new(
            "rdp_runner_args_invalid",
            "RDP runner 参数引号未闭合。",
            "unterminated quote",
            true,
        ));
    }
    if !current.is_empty() {
        reject_secret_arg(&current)?;
        args.push(current);
    }
    Ok(args)
}

fn reject_secret_arg(value: &str) -> Result<(), AppError> {
    let lowered = value.to_ascii_lowercase();
    for forbidden in ["password", "passwd", "/p:", "/pass:"] {
        if lowered.contains(forbidden) {
            return Err(AppError::new(
                "rdp_runner_args_secret_forbidden",
                "RDP runner 参数不能包含密码字段。",
                format!("arg contains {forbidden}"),
                true,
            ));
        }
    }
    Ok(())
}

fn current_platform() -> RdpPlatform {
    match env::consts::OS {
        "windows" => RdpPlatform::Windows,
        "linux" => RdpPlatform::Linux,
        "macos" => RdpPlatform::Macos,
        _ => RdpPlatform::Unknown,
    }
}

fn find_windows_mstsc() -> Option<PathBuf> {
    if let Some(path) = find_first_executable(&["mstsc.exe", "mstsc"]) {
        return Some(path);
    }
    env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("mstsc.exe"))
        .filter(|path| path.is_file())
}

fn find_windows_mstscax() -> Option<PathBuf> {
    env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("mstscax.dll"))
        .filter(|path| path.is_file())
        .or_else(|| find_first_executable(&["mstscax.dll"]))
}

fn find_macos_rdp_app_name() -> Option<String> {
    ["Windows App", "Microsoft Remote Desktop"]
        .into_iter()
        .find(|name| macos_app_bundle_exists(name))
        .map(str::to_string)
}

fn macos_app_bundle_exists(name: &str) -> bool {
    let bundle_name = format!("{name}.app");
    macos_application_dirs()
        .into_iter()
        .any(|dir| dir.join(&bundle_name).is_dir())
}

fn macos_application_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    dirs
}

fn macos_rdp_open_args(path_arg: String) -> Vec<String> {
    macos_rdp_open_args_with_app(path_arg, find_macos_rdp_app_name())
}

fn macos_rdp_open_args_with_app(path_arg: String, app_name: Option<String>) -> Vec<String> {
    if let Some(app_name) = app_name {
        return vec!["-a".to_string(), app_name, path_arg];
    }
    vec![path_arg]
}

fn find_custom_executable(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_file() {
        return Some(path);
    }
    find_first_executable(&[value])
}

fn find_first_executable(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| find_executable(name))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    let path_exts = executable_extensions();
    let path_value = env::var_os("PATH")?;
    env::split_paths(&path_value)
        .flat_map(|dir| candidate_executables(&dir, name, &path_exts))
        .find(|candidate| candidate.is_file())
}

fn executable_extensions() -> Vec<OsString> {
    if cfg!(windows) {
        env::var_os("PATHEXT")
            .map(|value| {
                env::split_paths(&value)
                    .map(|item| item.into_os_string())
                    .collect::<Vec<_>>()
            })
            .filter(|items| !items.is_empty())
            .unwrap_or_else(|| {
                vec![
                    OsString::from(".EXE"),
                    OsString::from(".BAT"),
                    OsString::from(".CMD"),
                ]
            })
    } else {
        vec![OsString::new()]
    }
}

fn candidate_executables(dir: &Path, name: &str, extensions: &[OsString]) -> Vec<PathBuf> {
    let base = dir.join(name);
    if Path::new(name).extension().is_some() || !cfg!(windows) {
        return vec![base];
    }
    extensions
        .iter()
        .map(|extension| {
            let mut candidate = base.clone().into_os_string();
            candidate.push(extension);
            PathBuf::from(candidate)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    #[ignore = "manual opt-in: opens a saved RDP connection in the production native host"]
    fn manual_native_keyboard_session() {
        use super::*;
        use tauri::Manager;
        let target =
            std::env::var("MXTERM_RDP_TEST_HOST").expect("set MXTERM_RDP_TEST_HOST explicitly");
        let mut context = tauri::generate_context!();
        context.config_mut().app.windows.clear();
        let app = tauri::Builder::default()
            .any_thread()
            .manage(crate::storage_vault::VaultState::default())
            .manage(RdpSessionManager::default())
            .build(context)
            .expect("test application");
        let app_root = app.path().app_data_dir().unwrap();
        app.state::<crate::storage_vault::VaultState>()
            .unlock_local(&app_root)
            .expect("local vault must already be available");
        let repository = StorageRepository::open_app(app.handle()).unwrap();
        let profiles = repository
            .connection_list()
            .unwrap()
            .into_iter()
            .filter(|p| p.protocol == ConnectionProtocol::Rdp && p.host == target)
            .collect::<Vec<_>>();
        assert_eq!(
            profiles.len(),
            1,
            "manual test requires one matching saved RDP profile"
        );
        let resolved = resolve_rdp_connection(app.handle(), &profiles[0].id).unwrap();
        assert!(matches!(
            resolved.rdp.runner.render_mode,
            RdpRenderMode::Embedded
        ));
        assert!(matches!(
            resolved.rdp.resources.keyboard_hook_mode,
            crate::connections::RdpKeyboardHookMode::Remote
        ));
        let appearance = std::env::var("MXTERM_RDP_TEST_APPEARANCE")
            .ok()
            .map(|json| {
                serde_json::from_str(&json).expect("valid native appearance test snapshot")
            });
        let launched = tauri::async_runtime::block_on(launch_connection(
            app.handle(),
            &app.state::<RdpSessionManager>(),
            RdpConnectionRequest {
                connection_id: profiles[0].id.clone(),
                bounds: None,
                appearance,
            },
        ))
        .unwrap();
        assert!(
            matches!(launched.runner, RdpRunnerKind::MstscActiveX),
            "standard host must not fall back"
        );
        eprintln!("[rdp-test] production WinForms host ready; close its window to finish");
        let pid = launched.process_id.unwrap();
        while app
            .state::<RdpSessionManager>()
            .native_host
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h.pid == pid && h.is_alive())
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    use super::{macos_rdp_open_args_with_app, serialize_rdp_file};
    use crate::connections::{
        ConnectionAdvancedConfig, ConnectionCredentialMode, ConnectionJumpConfig,
        ConnectionProfile, ConnectionProtocol, ConnectionProxyConfig, RdpCertificatePolicy,
        RdpConnectionConfig, RdpPerformanceConfig, RdpPerformancePreset, RdpSecurityConfig,
    };

    fn rdp_profile() -> ConnectionProfile {
        ConnectionProfile {
            id: "rdp-1".to_string(),
            name: "RDP 1".to_string(),
            protocol: ConnectionProtocol::Rdp,
            group: None,
            host: "192.0.2.40".to_string(),
            port: 3389,
            username: r"MicrosoftAccount\user@example.com".to_string(),
            credential_mode: ConnectionCredentialMode::Prompt,
            credential_id: None,
            inline_auth_kind: None,
            inline_password: None,
            inline_private_key_path: None,
            inline_private_key_passphrase: None,
            prompt_auth_kind: None,
            proxy: ConnectionProxyConfig::default(),
            jump: ConnectionJumpConfig::default(),
            advanced: ConnectionAdvancedConfig::default(),
            rdp: None,
            vnc: None,
            telnet: None,
            serial: None,
            notes: None,
            is_favorite: false,
            last_connected_at: None,
            remote_os_id: None,
            remote_os_name: None,
            remote_os_version: None,
            created_at: "2026-06-24T00:00:00+08:00".to_string(),
            updated_at: "2026-06-24T00:00:00+08:00".to_string(),
            auth_kind: None,
            password: None,
            private_key_path: None,
            private_key_passphrase: None,
        }
    }

    fn config_with_certificate_policy(policy: RdpCertificatePolicy) -> RdpConnectionConfig {
        RdpConnectionConfig {
            security: RdpSecurityConfig {
                certificate_policy: policy,
                ..RdpSecurityConfig::default()
            },
            ..RdpConnectionConfig::default()
        }
    }

    #[test]
    fn rdp_prompt_certificate_policy_allows_mstsc_warning_continue() {
        let content = serialize_rdp_file(
            &rdp_profile(),
            &config_with_certificate_policy(RdpCertificatePolicy::Prompt),
        )
        .unwrap();

        assert!(content.contains("authentication level:i:2\r\n"));
    }

    #[test]
    fn rdp_certificate_policy_maps_trust_and_strict() {
        let trust = serialize_rdp_file(
            &rdp_profile(),
            &config_with_certificate_policy(RdpCertificatePolicy::Trust),
        )
        .unwrap();
        let strict = serialize_rdp_file(
            &rdp_profile(),
            &config_with_certificate_policy(RdpCertificatePolicy::Strict),
        )
        .unwrap();

        assert!(trust.contains("authentication level:i:0\r\n"));
        assert!(strict.contains("authentication level:i:1\r\n"));
    }

    #[test]
    fn rdp_default_experience_enables_desktop_composition() {
        let content = serialize_rdp_file(&rdp_profile(), &RdpConnectionConfig::default()).unwrap();

        assert!(content.contains("smart sizing:i:1\r\n"));
        assert!(content.contains("session bpp:i:32\r\n"));
        assert!(content.contains("connection type:i:7\r\n"));
        assert!(content.contains("allow font smoothing:i:1\r\n"));
        assert!(content.contains("allow desktop composition:i:1\r\n"));
        assert!(content.contains("disable themes:i:0\r\n"));
        assert!(content.contains("disable full window drag:i:0\r\n"));
        assert!(content.contains("disable menu anims:i:0\r\n"));
    }

    #[test]
    fn rdp_file_applies_keyboard_hook_mode() {
        // 默认「在远程计算机上」→ keyboardhook:i:1
        let remote = serialize_rdp_file(&rdp_profile(), &RdpConnectionConfig::default()).unwrap();
        assert!(remote.contains("keyboardhook:i:1\r\n"));

        let local = serialize_rdp_file(
            &rdp_profile(),
            &RdpConnectionConfig {
                resources: crate::connections::RdpResourceConfig {
                    keyboard_hook_mode: crate::connections::RdpKeyboardHookMode::Local,
                    ..RdpConnectionConfig::default().resources
                },
                ..RdpConnectionConfig::default()
            },
        )
        .unwrap();
        assert!(local.contains("keyboardhook:i:0\r\n"));

        let fullscreen = serialize_rdp_file(
            &rdp_profile(),
            &RdpConnectionConfig {
                resources: crate::connections::RdpResourceConfig {
                    keyboard_hook_mode: crate::connections::RdpKeyboardHookMode::FullscreenOnly,
                    ..RdpConnectionConfig::default().resources
                },
                ..RdpConnectionConfig::default()
            },
        )
        .unwrap();
        assert!(fullscreen.contains("keyboardhook:i:2\r\n"));
    }

    #[test]
    fn rdp_low_bandwidth_experience_disables_desktop_composition() {
        let content = serialize_rdp_file(
            &rdp_profile(),
            &RdpConnectionConfig {
                performance: RdpPerformanceConfig {
                    preset: RdpPerformancePreset::LowBandwidth,
                    ..RdpPerformanceConfig::default()
                },
                ..RdpConnectionConfig::default()
            },
        )
        .unwrap();

        assert!(content.contains("session bpp:i:16\r\n"));
        assert!(content.contains("allow desktop composition:i:0\r\n"));
        assert!(content.contains("disable themes:i:1\r\n"));
        assert!(content.contains("disable cursor setting:i:1\r\n"));
    }

    #[test]
    fn rdp_fixed_resolution_does_not_enable_smart_sizing() {
        let content = serialize_rdp_file(
            &rdp_profile(),
            &RdpConnectionConfig {
                display: crate::connections::RdpDisplayConfig {
                    dynamic_resize: false,
                    ..RdpConnectionConfig::default().display
                },
                ..RdpConnectionConfig::default()
            },
        )
        .unwrap();

        assert!(!content.contains("smart sizing:i:1\r\n"));
    }

    #[test]
    fn macos_rdp_open_args_target_official_client_when_found() {
        let args = macos_rdp_open_args_with_app(
            "/tmp/example.rdp".to_string(),
            Some("Windows App".to_string()),
        );

        assert_eq!(args, vec!["-a", "Windows App", "/tmp/example.rdp"]);
    }

    #[test]
    fn macos_rdp_open_args_fall_back_to_default_handler() {
        let args = macos_rdp_open_args_with_app("/tmp/example.rdp".to_string(), None);

        assert_eq!(args, vec!["/tmp/example.rdp"]);
    }
}
