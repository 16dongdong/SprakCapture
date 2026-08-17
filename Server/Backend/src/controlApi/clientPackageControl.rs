//! 管理 Android 客户端任务、独立打包器进程、产物保留与 HTTP 下载。
//!
//! Client 与 Server 保持独立工程；桌面发布阶段把 Client 预编译为模板，本模块运行时通过 CLI 把节点、
//! 全随机安装身份和本次认证凭据交给 `clientPackager.exe`。凭据仅走子进程标准输入，不进入参数或记录。

use std::{
    collections::VecDeque,
    env, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use bytes::Bytes;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{RwLock, mpsc, oneshot},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use super::{ControlState, clientRulesHost, httpControl::LocalizedApiError};
use crate::localization::{ErrorCode, RequestLocale};

const maximumRetainedPackages: usize = 10;
const packageDirectoryName: &str = "clientPackages";
const signingDirectoryName: &str = "clientSigning";
const clientTemplateEnvironment: &str = "CAPTURE_CLIENT_TEMPLATE_PATH";
const clientPackagerEnvironment: &str = "CAPTURE_CLIENT_PACKAGER_EXECUTABLE";
const clientPackagerFileName: &str = "clientPackager.exe";
const clientPackagerTimeout: Duration = Duration::from_secs(60);
const clientPackagerTerminationTimeout: Duration = Duration::from_secs(5);
const maximumClientPackagerOutputBytes: usize = 64 * 1024;
const maximumClientDownloadRequestBytes: usize = 2 * 1024 * 1024;
const maximumClientIconBytes: usize = 1024 * 1024;
const clientDownloadChunkBytes: usize = 64 * 1024;
const clientDownloadBufferChunks: usize = 4;
const clientRulesPath: &str = "/api/v1/client/routing.txt";
const clientPublicHostEnvironment: &str = "CAPTURE_CLIENT_PUBLIC_HOST";
#[cfg(debug_assertions)]
const clientTestHostEnvironment: &str = "CAPTURE_CLIENT_TEST_HOST";
const publicIpDiscoveryUrl: &str = "https://api.ipify.org";
const publicIpDiscoveryTimeout: Duration = Duration::from_secs(5);
const clientPackageCleanupAttempts: usize = 8;
const clientPackageCleanupRetryDelay: Duration = Duration::from_millis(250);
const clientPackageBuildFailureReason: &str = "客户端生成失败，请检查节点、规则集和打包器状态";
const clientPackageProcessBlockedReason: &str =
    "客户端打包器退出状态无法确认，已阻止后续生成，请重启服务完成回收";
const clientPackageRecordFailureReason: &str = "客户端记录提交失败，临时产物已回收";
const clientPackageCleanupFailureReason: &str = "临时客户端清理失败，服务重启时将继续回收";
#[cfg(target_os = "windows")]
const windowsCreateNoWindow: u32 = 0x0800_0000;

/// 表示客户端生成任务的公开阶段；终态保留在快照中，仅用于刷新后查看结果或失败原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ClientPackageStage {
    Preparing,
    Building,
    Verifying,
    Ready,
    Failed,
}

/// 描述单次客户端生成任务；随机安装身份与公开节点可展示，但不包含任何认证材料。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientPackageJob {
    id: Uuid,
    stage: ClientPackageStage,
    applicationId: String,
    applicationName: String,
    /// 节点只供后台把本次请求传给打包器，快照序列化时必须隐藏，避免管理 API 暴露部署地址。
    #[serde(skip_serializing)]
    nodeHost: String,
    /// 端口与节点主机属于同一机密边界，不参与任务状态和前端记录输出。
    #[serde(skip_serializing)]
    nodePort: u16,
    startedAtMilliseconds: u64,
    failureReason: Option<String>,
}

/// 描述一次已完成生成记录；摘要用于本次下载后校验，文件系统路径和账号凭据永不进入协议。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientPackageArtifact {
    id: Uuid,
    applicationId: String,
    #[serde(default = "legacyApplicationName")]
    applicationName: String,
    createdAtMilliseconds: u64,
    fileName: String,
    sizeBytes: u64,
    sha256: String,
}

/// 为旧版脱敏记录补齐当时尚未持久化的软件名；该值只用于历史列表展示，不参与新包身份生成。
fn legacyApplicationName() -> String {
    "旧版客户端".to_owned()
}

/// 汇总当前生成任务与最近产物；列表按创建时间倒序且最多保留固定数量。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientPackageSnapshot {
    activeJob: Option<ClientPackageJob>,
    packages: Vec<ClientPackageArtifact>,
}

#[derive(Default)]
struct ClientPackageState {
    activeJob: Option<ClientPackageJob>,
    packages: VecDeque<ClientPackageArtifact>,
}

struct ClientBuildRequest {
    job: ClientPackageJob,
    packagerExecutable: PathBuf,
    templatePath: PathBuf,
    packageDirectory: PathBuf,
    signingDirectory: PathBuf,
    username: String,
    password: String,
    rulesUrl: String,
    iconBase64: Option<String>,
}

/// 下载页提交认证凭据与可选安装身份；凭据和图标不会复制到任务、产物元数据或公开快照。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientPackageDownloadRequest {
    username: String,
    password: String,
    applicationId: Option<String>,
    applicationName: Option<String>,
    iconBase64: Option<String>,
}

/// 聚合一次下载打包所需的节点、认证和安装身份；完整对象只移动进单次后台任务。
struct ClientPackageBuildInput {
    nodeHost: String,
    nodePort: u16,
    downloadRequest: ClientPackageDownloadRequest,
    rulesUrl: String,
}

/// 区分调度冲突与异步打包失败；内部原因只用于更新任务状态，不携带请求中的秘密。
enum ClientPackageBuildError {
    Busy,
    Failed,
}

/// 表示独立打包器失败以及子进程是否已确认退出；未确认退出时禁止删除其可能仍持有的文件。
struct ClientPackagerRunError {
    reason: String,
    processExited: bool,
}

/// 绑定一次构建的临时 APK；删除失败会进入有界后台重试，重试结束前始终持有单任务租约。
struct TemporaryClientPackage {
    path: PathBuf,
    manager: ClientPackageManager,
    operationLease: Option<ClientPackageOperationLease>,
}

impl TemporaryClientPackage {
    /// 创建已存在临时 APK 的唯一清理所有权；管理器用于把重试终态写入公开任务状态。
    fn new(
        path: PathBuf,
        manager: ClientPackageManager,
        operationLease: ClientPackageOperationLease,
    ) -> Self {
        Self {
            path,
            manager,
            operationLease: Some(operationLease),
        }
    }

    /// 在文件句柄关闭后删除 APK；Windows 文件锁等瞬态失败会重试并保持操作互斥。
    async fn remove(mut self) {
        let cleanup = self.takeCleanup().expect("临时 APK 清理所有权必须存在");
        retryTemporaryPackageCleanup(
            cleanup,
            clientPackageCleanupAttempts,
            clientPackageCleanupRetryDelay,
        )
        .await;
    }

    /// 转移唯一清理上下文；转移后 Drop 不得再次调度同一路径。
    fn takeCleanup(&mut self) -> Option<TemporaryPackageCleanup> {
        self.operationLease
            .take()
            .map(|operationLease| TemporaryPackageCleanup {
                path: self.path.clone(),
                manager: self.manager.clone(),
                _operationLease: operationLease,
            })
    }
}

/// 覆盖构建、响应建立和流式发送的完整单任务边界；析构一定释放下一次生成资格。
struct ClientPackageOperationLease {
    operationActive: Arc<AtomicBool>,
    releaseOnDrop: bool,
}

impl Drop for ClientPackageOperationLease {
    /// 无论任务成功、失败还是客户端断流，都在最后一个临时文件所有者销毁时释放互斥状态。
    fn drop(&mut self) {
        if self.releaseOnDrop {
            self.operationActive.store(false, Ordering::Release);
        }
    }
}

impl Drop for TemporaryClientPackage {
    /// 覆盖 HTTP 取消、任务 panic 和响应构造失败；同步删除失败后把租约转交异步重试任务。
    fn drop(&mut self) {
        let Some(cleanup) = self.takeCleanup() else {
            return;
        };
        match std::fs::remove_file(&cleanup.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    runtime.spawn(retryTemporaryPackageCleanup(
                        cleanup,
                        clientPackageCleanupAttempts,
                        clientPackageCleanupRetryDelay,
                    ));
                }
                Err(_) => {
                    eprintln!("临时客户端 APK 清理失败，服务下次启动将继续清理");
                }
            },
        }
    }
}

/// 保存后台清理所需的最小上下文；租约字段确保重试完成前不会启动下一次打包。
struct TemporaryPackageCleanup {
    path: PathBuf,
    manager: ClientPackageManager,
    _operationLease: ClientPackageOperationLease,
}

/// 后台构建成功只把记录和临时文件所有权交给当前下载请求，不把路径写入公开状态。
struct ReadyClientPackage {
    artifact: ClientPackageArtifact,
    temporaryPackage: TemporaryClientPackage,
}

/// 协调单实例 Android 模板装配；操作锁保证签名身份和产物不会被两个任务并发改写。
#[derive(Clone)]
pub(super) struct ClientPackageManager {
    state: Arc<RwLock<ClientPackageState>>,
    operationActive: Arc<AtomicBool>,
    packagerExecutable: Arc<PathBuf>,
    templatePath: Arc<PathBuf>,
    packageDirectory: Arc<PathBuf>,
    signingDirectory: Arc<PathBuf>,
}

impl ClientPackageManager {
    /// 从数据目录恢复最近生成记录、清理遗留 APK 并解析预编译 Client 模板位置。
    ///
    /// 运行上下文：控制服务初始化时调用一次；`dataDirectory` 是当前用户数据根。
    /// 失败语义：产物目录不可创建时阻止控制服务启动，损坏的单个元数据文件会返回明确 I/O 错误。
    pub(super) fn load(dataDirectory: &Path) -> io::Result<Self> {
        let packageDirectory = dataDirectory.join(packageDirectoryName);
        let signingDirectory = dataDirectory.join(signingDirectoryName);
        std::fs::create_dir_all(&packageDirectory)?;
        std::fs::create_dir_all(&signingDirectory)?;
        removeOrphanedPackageFiles(&packageDirectory)?;
        let packages = loadArtifacts(&packageDirectory)?;
        Ok(Self {
            state: Arc::new(RwLock::new(ClientPackageState {
                activeJob: None,
                packages,
            })),
            operationActive: Arc::new(AtomicBool::new(false)),
            packagerExecutable: Arc::new(resolveClientPackagerExecutable()),
            templatePath: Arc::new(resolveClientTemplatePath()),
            packageDirectory: Arc::new(packageDirectory),
            signingDirectory: Arc::new(signingDirectory),
        })
    }

    /// 返回内存中的任务与产物快照；只读操作不访问模板或签名文件。
    pub(super) async fn snapshot(&self) -> ClientPackageSnapshot {
        let state = self.state.read().await;
        ClientPackageSnapshot {
            activeJob: state.activeJob.clone(),
            packages: state.packages.iter().cloned().collect(),
        }
    }

    /// 创建唯一包名并异步启动一次构建；调用者等待结果时断开 HTTP 也不会中止状态收敛。
    ///
    /// 参数中的账号密码只移动进独立后台任务，任务与产物模型永不保存。已有构建返回 Busy；打包失败
    /// 返回 Failed 且同步写入公开任务失败原因，防止下载请求断开后留下永久 Building 状态。
    async fn buildForDownload(
        &self,
        input: ClientPackageBuildInput,
    ) -> Result<ReadyClientPackage, ClientPackageBuildError> {
        let ClientPackageBuildInput {
            nodeHost,
            nodePort,
            downloadRequest,
            rulesUrl,
        } = input;
        if self
            .operationActive
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ClientPackageBuildError::Busy);
        }
        let operationLease = ClientPackageOperationLease {
            operationActive: Arc::clone(&self.operationActive),
            releaseOnDrop: true,
        };
        let jobId = Uuid::new_v4();
        let applicationId = downloadRequest
            .applicationId
            .clone()
            .unwrap_or_else(|| randomApplicationId(jobId));
        let applicationName = downloadRequest
            .applicationName
            .clone()
            .unwrap_or_else(|| randomApplicationName(jobId));
        let job = ClientPackageJob {
            id: jobId,
            stage: ClientPackageStage::Preparing,
            applicationId,
            applicationName,
            nodeHost,
            nodePort,
            startedAtMilliseconds: currentTimeMilliseconds(),
            failureReason: None,
        };
        {
            let mut state = self.state.write().await;
            state.activeJob = Some(job.clone());
        }

        let manager = self.clone();
        let request = ClientBuildRequest {
            job: job.clone(),
            packagerExecutable: self.packagerExecutable.as_ref().clone(),
            templatePath: self.templatePath.as_ref().clone(),
            packageDirectory: self.packageDirectory.as_ref().clone(),
            signingDirectory: self.signingDirectory.as_ref().clone(),
            username: downloadRequest.username,
            password: downloadRequest.password,
            rulesUrl,
            iconBase64: downloadRequest.iconBase64,
        };
        let (completionSender, completionReceiver) = oneshot::channel();
        tokio::spawn(async move {
            let mut operationLease = Some(operationLease);
            let buildResult = match buildClientPackage(&manager, &request).await {
                Ok(artifact) => {
                    let temporaryPackage = TemporaryClientPackage::new(
                        request
                            .packageDirectory
                            .join(packageFileName(request.job.id)),
                        manager.clone(),
                        operationLease.take().expect("打包操作租约必须仅移动一次"),
                    );
                    match manager.complete(artifact.clone()).await {
                        Ok(()) => Ok(ReadyClientPackage {
                            artifact,
                            temporaryPackage,
                        }),
                        Err(mut reason) => {
                            if let Err(rollbackError) =
                                removeArtifactRecord(&request.packageDirectory, &artifact).await
                            {
                                reason.push_str(&format!("；回滚生成记录失败：{rollbackError}"));
                            }
                            drop(temporaryPackage);
                            // 详细存储错误可能包含本机路径，只参与当前回滚；公开任务状态使用固定诊断。
                            drop(reason);
                            manager.fail(clientPackageRecordFailureReason).await;
                            Err(())
                        }
                    }
                }
                Err(buildError) => {
                    // 打包器 stderr 和 I/O 错误可能包含路径，禁止原样进入远程快照或日志。
                    let processExited = buildError.processExited;
                    drop(buildError.reason);
                    manager
                        .fail(if processExited {
                            clientPackageBuildFailureReason
                        } else {
                            clientPackageProcessBlockedReason
                        })
                        .await;
                    finishFailedOperation(
                        operationLease.take().expect("失败任务必须仍持有操作租约"),
                        processExited,
                    );
                    Err(())
                }
            };
            let _ = completionSender.send(buildResult);
        });
        completionReceiver
            .await
            .map_err(|_| ClientPackageBuildError::Failed)?
            .map_err(|()| ClientPackageBuildError::Failed)
    }

    /// 推进当前任务阶段；任务 ID 不匹配时保持现状，防止迟到任务覆盖新任务状态。
    async fn setStage(&self, id: Uuid, stage: ClientPackageStage) {
        let mut state = self.state.write().await;
        if let Some(job) = state.activeJob.as_mut().filter(|job| job.id == id) {
            job.stage = stage;
        }
    }

    /// 提交已验证生成记录并严格裁剪历史；APK 仍由当前响应独占，记录只保存非秘密摘要。
    ///
    /// 运行上下文：仅在完整生成操作租约内调用；`artifact` 已完成签名和摘要验证。任一旧记录删除失败时
    /// 任务转为失败，禁止通过忽略 I/O 错误制造重启后重新出现的幽灵记录。
    async fn complete(&self, artifact: ClientPackageArtifact) -> Result<(), String> {
        let staleArtifacts = {
            let state = self.state.read().await;
            state
                .packages
                .iter()
                .skip(maximumRetainedPackages.saturating_sub(1))
                .cloned()
                .collect::<Vec<_>>()
        };
        for staleArtifact in &staleArtifacts {
            removeArtifactRecord(&self.packageDirectory, staleArtifact).await?;
        }

        let staleIds = staleArtifacts
            .iter()
            .map(|staleArtifact| staleArtifact.id)
            .collect::<Vec<_>>();
        let mut state = self.state.write().await;
        state
            .packages
            .retain(|existingArtifact| !staleIds.contains(&existingArtifact.id));
        state.packages.push_front(artifact.clone());
        if let Some(job) = state.activeJob.as_mut().filter(|job| job.id == artifact.id) {
            job.stage = ClientPackageStage::Ready;
        }
        Ok(())
    }

    /// 记录可远程展示的固定失败原因；参数只能来自本模块静态文案，动态路径和子进程输出不会进入快照。
    async fn fail(&self, reason: &'static str) {
        let mut state = self.state.write().await;
        if let Some(job) = state.activeJob.as_mut() {
            job.stage = ClientPackageStage::Failed;
            job.failureReason = Some(reason.to_owned());
        }
    }
}

/// 结束失败任务的互斥所有权；退出未确认时保留门闩到进程重启，禁止残留打包器与新任务并发写文件。
fn finishFailedOperation(mut lease: ClientPackageOperationLease, processExited: bool) {
    lease.releaseOnDrop = processExited;
}

/// 装配客户端记录与一次性认证下载路由；历史记录不提供二次下载，避免泄露其中的内置凭据。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route("/api/v1/clientPackages", get(getClientPackages))
        .route(
            "/api/v1/clientPackages/download",
            post(downloadGeneratedClientPackage)
                .layer(DefaultBodyLimit::max(maximumClientDownloadRequestBytes)),
        )
}

/// 返回当前任务和最近产物，不推进控制面 revision。
async fn getClientPackages(State(state): State<ControlState>) -> Json<ClientPackageSnapshot> {
    Json(state.clientPackages.snapshot().await)
}

/// 校验 SOCKS5 账号后同步等待独立打包任务，并流式返回本次随机 APK。
///
/// 运行上下文：公开 `/client` 下载页和本机控制端口共用此入口，因此这里始终执行账号服务权威校验，
/// 不能信任外层转发。无效账号返回 401，远程规则入口不可用返回 503，单任务冲突返回 409。
async fn downloadGeneratedClientPackage(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Json(mut downloadRequest): Json<ClientPackageDownloadRequest>,
) -> Result<Response, LocalizedApiError> {
    normalizeClientPackageCustomization(&mut downloadRequest).map_err(|()| {
        super::ApiError::badRequest(ErrorCode::InvalidConfigurationRequest).withLocale(locale)
    })?;
    // 无固定密码账号仍要求下载者提交任意非空密码，使 APK、SOCKS RFC1929 与规则 HTTP Basic
    // 始终共享同一组可序列化凭据；空字段在进入账号服务和任务状态前统一按认证失败处理。
    if downloadRequest.username.is_empty() || downloadRequest.password.is_empty() {
        return Err(
            super::ApiError::unauthorized(ErrorCode::ClientPackageAuthenticationFailed)
                .withLocale(locale),
        );
    }
    let credentialsAccepted = state
        .accountService
        .verifyClientCredentials(&downloadRequest.username, &downloadRequest.password)
        .await
        .map_err(|_| {
            super::ApiError::unavailable(ErrorCode::ClientPackageServiceUnavailable)
                .withLocale(locale)
        })?;
    if !credentialsAccepted {
        return Err(
            super::ApiError::unauthorized(ErrorCode::ClientPackageAuthenticationFailed)
                .withLocale(locale),
        );
    }
    let activeRuleSetAvailable = state
        .accountService
        .activeClientRuleSetAvailable()
        .await
        .map_err(|_| {
            super::ApiError::unavailable(ErrorCode::ClientPackageServiceUnavailable)
                .withLocale(locale)
        })?;
    if !activeRuleSetAvailable {
        return Err(
            super::ApiError::unavailable(ErrorCode::ClientPackageServiceUnavailable)
                .withLocale(locale),
        );
    }
    let configuration = state.configuration.read().await;
    let nodeHost = resolvePackagedNodeHost(configuration.listenHost)
        .await
        .map_err(|code| super::ApiError::badRequest(code).withLocale(locale))?;
    let nodePort = configuration.listenPort;
    drop(configuration);
    let multiAccountConfiguration = state.multiAccountConfiguration.read().await;
    if !multiAccountConfiguration.enabled {
        return Err(
            super::ApiError::unavailable(ErrorCode::ClientPackageServiceUnavailable)
                .withLocale(locale),
        );
    }
    let rulesUrl = clientRulesUrl(multiAccountConfiguration.remotePort);
    drop(multiAccountConfiguration);
    let readyPackage = state
        .clientPackages
        .buildForDownload(ClientPackageBuildInput {
            nodeHost,
            nodePort,
            downloadRequest,
            rulesUrl,
        })
        .await
        .map_err(|error| match error {
            ClientPackageBuildError::Busy => {
                super::ApiError::conflict(ErrorCode::ClientPackageBusy).withLocale(locale)
            }
            ClientPackageBuildError::Failed => {
                super::ApiError::internal(ErrorCode::ClientPackageOperationFailed)
                    .withLocale(locale)
            }
        })?;
    let ReadyClientPackage {
        artifact,
        temporaryPackage,
    } = readyPackage;
    // 临时文件所有权先于文件句柄建立；请求在此后被取消时，Rust 逆序析构会先关闭句柄再删除 APK。
    let file = fs::File::open(&temporaryPackage.path)
        .await
        .map_err(|error| {
            // 系统错误会携带完整本机路径；公开响应只返回稳定错误码，临时产物由所有权析构回收。
            drop(error);
            super::ApiError::internal(ErrorCode::ClientPackageOperationFailed).withLocale(locale)
        })?;
    let disposition = format!("attachment; filename=\"{}\"", artifact.fileName);
    let (downloadSender, downloadReceiver) = mpsc::channel(clientDownloadBufferChunks);
    tokio::spawn(streamTemporaryPackage(
        file,
        temporaryPackage,
        downloadSender,
    ));
    let downloadStream = stream::unfold(downloadReceiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    let mut response = Body::from_stream(downloadStream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.android.package-archive"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&artifact.sizeBytes.to_string()).expect("APK 长度必须是合法响应头"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).expect("生成的 APK 文件名必须是合法响应头"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// 规范化并校验可选安装身份；空字符串表示使用服务端随机值，非空值必须能被 Android 无损接受。
///
/// 运行上下文：账号认证和打包任务创建前调用，避免非法图片占用单任务锁或进入子进程。
/// 失败返回 `Err(())`，公开层只映射稳定 400 错误码，不回显用户提交的包名、名称或图片正文。
fn normalizeClientPackageCustomization(
    request: &mut ClientPackageDownloadRequest,
) -> Result<(), ()> {
    normalizeOptionalText(&mut request.applicationId);
    normalizeOptionalText(&mut request.applicationName);
    if request
        .applicationId
        .as_deref()
        .is_some_and(|value| !isValidApplicationId(value))
    {
        return Err(());
    }
    if request
        .applicationName
        .as_deref()
        .is_some_and(|value| !isValidApplicationName(value))
    {
        return Err(());
    }
    if request.iconBase64.as_deref() == Some("") {
        request.iconBase64 = None;
    }
    if let Some(iconBase64) = request.iconBase64.as_deref() {
        let iconBytes = base64Standard.decode(iconBase64).map_err(|_| ())?;
        if iconBytes.is_empty() || iconBytes.len() > maximumClientIconBytes {
            return Err(());
        }
    }
    Ok(())
}

/// 把仅含空白的可选文本折叠为 None；非空输入保持原样，让后续校验明确拒绝首尾空白而不是静默改写身份。
fn normalizeOptionalText(value: &mut Option<String>) {
    let Some(current) = value.take() else {
        return;
    };
    if !current.trim().is_empty() {
        *value = Some(current);
    }
}

/// 校验用户自定义 Android applicationId；随机和自定义值共享同一小写 ASCII 语法。
fn isValidApplicationId(applicationId: &str) -> bool {
    let segments = applicationId.split('.').collect::<Vec<_>>();
    applicationId.len() <= 127
        && (2..=8).contains(&segments.len())
        && segments.iter().all(|segment| {
            segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

/// 校验用户自定义软件名；允许本地语言，但拒绝控制字符和会被浏览器悄悄裁剪的首尾空白。
fn isValidApplicationName(applicationName: &str) -> bool {
    applicationName.trim() == applicationName
        && (1..=32).contains(&applicationName.chars().count())
        && !applicationName.chars().any(char::is_control)
}

/// 把临时 APK 分块送入响应并在 EOF、读错误或客户端断流后删除文件。
///
/// 接收端被丢弃会使 `send` 立即失败；函数随后显式关闭文件句柄再清理路径，满足 Windows 删除语义。
async fn streamTemporaryPackage(
    mut file: fs::File,
    temporaryPackage: TemporaryClientPackage,
    sender: mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let mut chunk = vec![0_u8; clientDownloadChunkBytes];
    loop {
        match file.read(&mut chunk).await {
            Ok(0) => break,
            Ok(readBytes) => {
                if sender
                    .send(Ok(Bytes::copy_from_slice(&chunk[..readBytes])))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                break;
            }
        }
    }
    drop(file);
    temporaryPackage.remove().await;
}

/// 有界重试删除包含凭据的临时 APK；终态失败会写入任务快照并输出不含路径和凭据的诊断。
///
/// 运行上下文：正常下载结束、客户端断流以及 Drop 补偿路径共用此函数。`cleanup` 持有操作租约，
/// 因此重试期间新建请求只能得到 409；达到上限后文件留给下次启动扫描，失败原因对管理页面可见。
async fn retryTemporaryPackageCleanup(
    cleanup: TemporaryPackageCleanup,
    maximumAttempts: usize,
    retryDelay: Duration,
) {
    let mut lastError = None;
    for attempt in 1..=maximumAttempts.max(1) {
        match fs::remove_file(&cleanup.path).await {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => lastError = Some(error),
        }
        if attempt < maximumAttempts.max(1) {
            tokio::time::sleep(retryDelay).await;
        }
    }

    // 删除错误可能携带本机路径；只确认确有失败，不把系统诊断复制到远程快照或标准错误。
    drop(lastError.expect("至少一次删除失败必须保留错误"));
    eprintln!("{clientPackageCleanupFailureReason}");
    cleanup
        .manager
        .fail(clientPackageCleanupFailureReason)
        .await;
}

/// 调用独立打包器完成模板注入和签名，再保存下载元数据；运行时不启动 Gradle、JDK 或 Android SDK。
async fn buildClientPackage(
    manager: &ClientPackageManager,
    request: &ClientBuildRequest,
) -> Result<ClientPackageArtifact, ClientPackagerRunError> {
    manager
        .setStage(request.job.id, ClientPackageStage::Building)
        .await;
    let fileName = packageFileName(request.job.id);
    let destination = request.packageDirectory.join(&fileName);
    let packageResult = match runClientPackager(request, &destination).await {
        Ok(result) => result,
        Err(packagerError) => {
            // 只有确认子进程退出后才能清理 raw、signed 和目标 APK，避免 Windows 文件锁制造假清理结果。
            if !packagerError.processExited {
                return Err(packagerError);
            }
            let cleanupResult = removeTransientPackageFiles(&destination).await;
            return match cleanupResult {
                Ok(()) => Err(packagerError),
                Err(cleanupError) => Err(exitedPackagerError(format!(
                    "{}；清理客户端临时文件失败：{cleanupError}",
                    packagerError.reason
                ))),
            };
        }
    };
    manager
        .setStage(request.job.id, ClientPackageStage::Verifying)
        .await;
    let packageBytes = match fs::read(&destination).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(cleanupFailedClientPackage(
                &destination,
                format!("读取独立打包器产物失败：{error}"),
            )
            .await);
        }
    };
    let digest = hex::encode(Sha256::digest(&packageBytes));
    if packageResult.sizeBytes != packageBytes.len() as u64 || packageResult.sha256 != digest {
        return Err(cleanupFailedClientPackage(
            &destination,
            "独立打包器返回的摘要与 APK 文件不一致".to_owned(),
        )
        .await);
    }
    let artifact = ClientPackageArtifact {
        id: request.job.id,
        applicationId: request.job.applicationId.clone(),
        applicationName: request.job.applicationName.clone(),
        createdAtMilliseconds: currentTimeMilliseconds(),
        fileName,
        sizeBytes: packageBytes.len() as u64,
        sha256: digest,
    };
    let metadata = match serde_json::to_vec_pretty(&artifact) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(cleanupFailedClientPackage(
                &destination,
                format!("序列化 APK 元数据失败：{error}"),
            )
            .await);
        }
    };
    if let Err(error) = fs::write(
        metadataPath(&request.packageDirectory, artifact.id),
        metadata,
    )
    .await
    {
        return Err(cleanupFailedClientPackage(
            &destination,
            format!("保存 APK 元数据失败：{error}"),
        )
        .await);
    }
    Ok(artifact)
}

/// 清理失败构建产生的全部 APK 并合并原始原因；聚合后的错误由任务快照统一展示。
async fn cleanupFailedClientPackage(destination: &Path, reason: String) -> ClientPackagerRunError {
    match removeTransientPackageFiles(destination).await {
        Ok(()) => exitedPackagerError(reason),
        Err(cleanupError) => {
            exitedPackagerError(format!("{reason}；清理客户端临时文件失败：{cleanupError}"))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientPackagerOutput {
    sizeBytes: u64,
    sha256: String,
}

/// 独立打包器标准输入协议；全部运行期定制字段只写匿名管道，不进入进程列表、磁盘或失败格式化。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientPackagerInput<'a> {
    applicationId: &'a str,
    applicationName: &'a str,
    nodeHost: &'a str,
    nodePort: u16,
    username: &'a str,
    password: &'a str,
    rulesUrl: &'a str,
    iconBase64: Option<&'a str>,
}

/// 通过独立进程协议调用客户端打包器；主服务只负责参数、超时和结果校验，不链接 APK 签名实现。
///
/// 运行上下文：单实例生成锁内异步执行；`request` 携带受信路径和当前公开节点，`destination`
/// 使用服务生成的 UUID 文件名。启动失败、超时、非零退出、超限输出或非法 JSON 均进入任务失败原因。
async fn runClientPackager(
    request: &ClientBuildRequest,
    destination: &Path,
) -> Result<ClientPackagerOutput, ClientPackagerRunError> {
    let mut command = Command::new(&request.packagerExecutable);
    command
        .arg("package")
        .arg("--template")
        .arg(&request.templatePath)
        .arg("--output")
        .arg(destination)
        .arg("--signing-directory")
        .arg(&request.signingDirectory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.as_std_mut().creation_flags(windowsCreateNoWindow);

    let secretInput = Zeroizing::new(
        serde_json::to_vec(&ClientPackagerInput {
            applicationId: &request.job.applicationId,
            applicationName: &request.job.applicationName,
            nodeHost: &request.job.nodeHost,
            nodePort: request.job.nodePort,
            username: &request.username,
            password: &request.password,
            rulesUrl: &request.rulesUrl,
            iconBase64: request.iconBase64.as_deref(),
        })
        .map_err(|_| ClientPackagerRunError {
            reason: "序列化独立客户端打包器秘密输入失败".to_owned(),
            processExited: true,
        })?,
    );
    let output = executeClientPackager(
        command,
        secretInput,
        clientPackagerTimeout,
        clientPackagerTerminationTimeout,
    )
    .await?;
    if output.stdout.len() > maximumClientPackagerOutputBytes
        || output.stderr.len() > maximumClientPackagerOutputBytes
    {
        return Err(exitedPackagerError("独立客户端打包器输出超过协议上限"));
    }
    if !output.status.success() {
        let reason = String::from_utf8(output.stderr)
            .map_err(|_| exitedPackagerError("独立客户端打包器错误输出不是 UTF-8"))?;
        return Err(exitedPackagerError(format!(
            "独立客户端打包器失败：{}",
            reason.trim()
        )));
    }
    if !output.stderr.is_empty() {
        return Err(exitedPackagerError("独立客户端打包器成功时写入了错误输出"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| exitedPackagerError(format!("独立客户端打包器返回了无效 JSON：{error}")))
}

/// 执行已配置的独立打包器并并发排空输出；超时后显式终止并确认退出，调用方随后才可删除 APK。
async fn executeClientPackager(
    mut command: Command,
    secretInput: Zeroizing<Vec<u8>>,
    executionTimeout: Duration,
    terminationTimeout: Duration,
) -> Result<Output, ClientPackagerRunError> {
    let mut child = command.spawn().map_err(|error| ClientPackagerRunError {
        reason: format!("启动独立客户端打包器失败：{error}"),
        processExited: true,
    })?;
    let Some(mut childStdin) = child.stdin.take() else {
        return Err(terminateSpawnedPackager(
            &mut child,
            "独立客户端打包器标准输入管道未建立".to_owned(),
            terminationTimeout,
        )
        .await);
    };
    let Some(childStdout) = child.stdout.take() else {
        return Err(terminateSpawnedPackager(
            &mut child,
            "独立客户端打包器标准输出管道未建立".to_owned(),
            terminationTimeout,
        )
        .await);
    };
    let Some(childStderr) = child.stderr.take() else {
        return Err(terminateSpawnedPackager(
            &mut child,
            "独立客户端打包器错误输出管道未建立".to_owned(),
            terminationTimeout,
        )
        .await);
    };

    let execution = async {
        let input = async {
            childStdin
                .write_all(&secretInput)
                .await
                .map_err(|error| format!("写入独立客户端打包器秘密输入失败：{error}"))?;
            // Windows 管道只有关闭写端句柄才会向子进程发送 EOF；仅调用 AsyncWriteExt::shutdown
            // 不会释放句柄，打包器会一直等待 JSON 尾部，最终被 60 秒超时终止。
            drop(childStdin);
            Ok::<(), String>(())
        };
        let wait = child.wait();
        let stdout = readClientPackagerOutput(childStdout);
        let stderr = readClientPackagerOutput(childStderr);
        tokio::join!(input, wait, stdout, stderr)
    };

    match tokio::time::timeout(executionTimeout, execution).await {
        Ok((inputResult, statusResult, stdoutResult, stderrResult)) => {
            let status = match statusResult {
                Ok(status) => status,
                Err(error) => {
                    return Err(terminateSpawnedPackager(
                        &mut child,
                        format!("等待独立客户端打包器失败：{error}"),
                        terminationTimeout,
                    )
                    .await);
                }
            };
            let result = inputResult.and_then(|()| {
                Ok(Output {
                    status,
                    stdout: stdoutResult?,
                    stderr: stderrResult?,
                })
            });
            result.map_err(exitedPackagerError)
        }
        Err(_) => Err(terminateSpawnedPackager(
            &mut child,
            "独立客户端打包器执行超时".to_owned(),
            terminationTimeout,
        )
        .await),
    }
}

/// 处理进程创建后的任意失败：先终止并有界确认退出，再决定调用方是否能够清理本任务文件。
async fn terminateSpawnedPackager(
    child: &mut tokio::process::Child,
    reason: String,
    waitTimeout: Duration,
) -> ClientPackagerRunError {
    match terminateClientPackager(child, waitTimeout).await {
        Ok(()) => exitedPackagerError(reason),
        Err(terminationError) => ClientPackagerRunError {
            reason: format!("{reason}；{terminationError}"),
            processExited: false,
        },
    }
}

/// 限长读取独立打包器输出；读取上限加一字节用于区分恰好满载与真正越界。
async fn readClientPackagerOutput(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .take((maximumClientPackagerOutputBytes + 1) as u64)
        .read_to_end(&mut output)
        .await
        .map_err(|error| format!("读取独立客户端打包器输出失败：{error}"))?;
    if output.len() > maximumClientPackagerOutputBytes {
        return Err("独立客户端打包器输出超过协议上限".to_owned());
    }
    Ok(output)
}

/// 显式终止超时打包器并有界等待退出；等待失败时返回未确认状态，禁止调用方立即清理文件。
async fn terminateClientPackager(
    child: &mut tokio::process::Child,
    waitTimeout: Duration,
) -> Result<(), String> {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ok(());
    }
    let killError = child.start_kill().err().map(|error| error.to_string());
    let waitResult = tokio::time::timeout(waitTimeout, async {
        loop {
            match child.wait().await {
                Ok(_) => return,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    })
    .await;
    match waitResult {
        Ok(()) => Ok(()),
        Err(_) => {
            let killDetail = killError
                .map(|error| format!("；终止请求失败：{error}"))
                .unwrap_or_default();
            Err(format!(
                "等待已终止打包器退出超时，服务已阻止后续生成{killDetail}"
            ))
        }
    }
}

/// 创建已确认无存活子进程的失败值；调用方可安全遍历并删除本次 APK 临时文件。
fn exitedPackagerError(reason: impl Into<String>) -> ClientPackagerRunError {
    ClientPackagerRunError {
        reason: reason.into(),
        processExited: true,
    }
}

/// 解析独立打包器路径；桌面安装显式注入资源路径，直接运行后端时使用同目录可执行文件。
fn resolveClientPackagerExecutable() -> PathBuf {
    if let Some(path) = env::var_os(clientPackagerEnvironment) {
        return PathBuf::from(path);
    }
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join(clientPackagerFileName))
        })
        .unwrap_or_else(|| PathBuf::from(clientPackagerFileName))
}
/// 从显式环境或服务同目录解析预编译模板；运行时不访问源码、SDK 或 Gradle 缓存。
fn resolveClientTemplatePath() -> PathBuf {
    if let Some(path) = env::var_os(clientTemplateEnvironment) {
        return PathBuf::from(path);
    }
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("clientTemplate.apk"))
        })
        .unwrap_or_else(|| PathBuf::from("clientTemplate.apk"))
}

/// 解析写入 APK 的外部节点地址；监听地址只描述本机绑定，不能把局域网接口误当公网入口。
///
/// 运行上下文：凭据和规则预检通过后、启动打包器前调用。固定部署可用
/// `CAPTURE_CLIENT_PUBLIC_HOST` 指定公网 IP；显式公网监听直接复用，其余情况通过 ipify 的 HTTPS IPv4
/// 端点读取当前公网地址。调试构建另提供仅供隔离局域网验收的 `CAPTURE_CLIENT_TEST_HOST`，发布构建不含该分支。
/// 环境值、响应或网络无效时返回 `ClientNodeUnavailable`，绝不把私网地址静默写入生产 APK。
async fn resolvePackagedNodeHost(listenHost: IpAddr) -> Result<String, ErrorCode> {
    if let Some(configuredHost) = env::var_os(clientPublicHostEnvironment) {
        return parsePublicClientNodeAddress(&configuredHost.to_string_lossy());
    }
    #[cfg(debug_assertions)]
    if let Some(testHost) = env::var_os(clientTestHostEnvironment) {
        let testAddress = testHost
            .to_string_lossy()
            .parse::<IpAddr>()
            .map_err(|_| ErrorCode::ClientNodeUnavailable)?;
        if testAddress.is_unspecified() || testAddress.is_loopback() {
            return Err(ErrorCode::ClientNodeUnavailable);
        }
        return Ok(testAddress.to_string());
    }
    if isPublicClientNodeAddress(listenHost) {
        return Ok(listenHost.to_string());
    }
    let response = reqwest::Client::builder()
        .timeout(publicIpDiscoveryTimeout)
        .build()
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?
        .get(publicIpDiscoveryUrl)
        .send()
        .await
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?
        .error_for_status()
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?;
    let address = response
        .text()
        .await
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?;
    isPublicClientNodeAddress(address)
        .then(|| address.to_string())
        .ok_or(ErrorCode::ClientNodeUnavailable)
}

/// 解析固定部署提供的公网 IP；该纯函数供运行时与回归测试共享，私网或保留地址统一返回节点不可用。
fn parsePublicClientNodeAddress(value: &str) -> Result<String, ErrorCode> {
    let address = value
        .parse::<IpAddr>()
        .map_err(|_| ErrorCode::ClientNodeUnavailable)?;
    isPublicClientNodeAddress(address)
        .then(|| address.to_string())
        .ok_or(ErrorCode::ClientNodeUnavailable)
}

/// 判断地址是否可作为默认外部节点；私网、链路本地、文档网段和组播都不能自动写入远端 APK。
fn isPublicClientNodeAddress(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_broadcast()
                && !address.is_documentation()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !isSpecialIpv4(address)
        }
        IpAddr::V6(address) => address.to_ipv4().map_or_else(
            || {
                !address.is_loopback()
                    && !address.is_unspecified()
                    && !address.is_unique_local()
                    && !address.is_unicast_link_local()
                    && !address.is_multicast()
                    && !isDeprecatedSiteLocalIpv6(address)
                    && !isDocumentationIpv6(address)
            },
            |mappedAddress| isPublicClientNodeAddress(IpAddr::V4(mappedAddress)),
        ),
    }
}

/// 识别标准库未完整覆盖的共享、协议分配、基准测试和保留 IPv4 网段。
fn isSpecialIpv4(address: Ipv4Addr) -> bool {
    let [first, second, _, _] = address.octets();
    first == 0
        || (first == 100 && (64..=127).contains(&second))
        || (first == 192 && second == 0)
        || (first == 198 && (18..=19).contains(&second))
        || first >= 240
}

/// 识别 RFC 3849 的 `2001:db8::/32` 文档地址；该网段不得被误判为可部署公网节点。
fn isDocumentationIpv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
}

/// 识别已废弃但仍非公网可路由的 `fec0::/10` 站点本地地址，避免旧格式地址进入发布节点。
fn isDeprecatedSiteLocalIpv6(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfec0
}

/// 判断远程管理监听是否实际覆盖客户端节点地址；仅同地址族通配地址或完全相同的具体地址有效。
///
/// 生成只在已认证 SOCKS5 内部可解析的绝对规则地址；保留域名阻止客户端绕过代理直接访问管理端口。
fn clientRulesUrl(remotePort: u16) -> String {
    format!("http://{clientRulesHost}:{remotePort}{clientRulesPath}")
}

/// 启动时删除上次进程中断留下的全部 APK；持久目录只允许保留不含秘密的 JSON 生成记录。
fn removeOrphanedPackageFiles(packageDirectory: &Path) -> io::Result<()> {
    let mut failures = Vec::new();
    for entryResult in std::fs::read_dir(packageDirectory)? {
        let entry = match entryResult {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(format!("读取目录项失败：{error}"));
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("apk") {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            failures.push(format!("{}：{error}", path.display()));
        }
    }
    packageCleanupResult(failures)
}

/// 清理本次独立打包器可能留下的目标、raw 与 signed APK；路径只由任务目标派生，绝不遍历或删除其他任务。
async fn removeTransientPackageFiles(destination: &Path) -> io::Result<()> {
    let mut failures = Vec::new();
    for path in transientPackageFiles(destination)? {
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{}：{error}", path.display())),
        }
    }
    packageCleanupResult(failures)
}

/// 按 ClientPackager 的稳定命名合同派生本任务三个候选文件；非 UTF-8 文件名会在启动子进程前失败。
fn transientPackageFiles(destination: &Path) -> io::Result<[PathBuf; 3]> {
    let parentDirectory = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "客户端产物路径缺少父目录"))?;
    let destinationName = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "客户端产物文件名不是 UTF-8"))?;
    Ok([
        destination.to_path_buf(),
        parentDirectory.join(format!(".{destinationName}.raw.apk")),
        parentDirectory.join(format!(".{destinationName}.signed.apk")),
    ])
}

/// 将完整清理遍历的失败列表转换为单个 I/O 错误；空列表代表所有候选均已删除或不存在。
fn packageCleanupResult(failures: Vec<String>) -> io::Result<()> {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("；")))
    }
}

/// 从持久目录恢复有效生成记录并按创建时间倒序排序；损坏元数据返回错误，避免静默隐藏历史漂移。
fn loadArtifacts(packageDirectory: &Path) -> io::Result<VecDeque<ClientPackageArtifact>> {
    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(packageDirectory)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let artifact: ClientPackageArtifact = serde_json::from_slice(&std::fs::read(&path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        validateStoredArtifact(&artifact)?;
        artifacts.push(artifact);
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.createdAtMilliseconds));
    artifacts.truncate(maximumRetainedPackages);
    Ok(artifacts.into())
}

/// 校验持久化记录的安装身份、显示文件名和摘要形态；记录不要求也不允许对应 APK 长期存在。
///
/// 运行上下文：控制服务启动时检查有界 JSON；`artifact` 来自磁盘且只能投影为只读历史记录。
fn validateStoredArtifact(artifact: &ClientPackageArtifact) -> io::Result<()> {
    if artifact.fileName != packageFileName(artifact.id)
        || !isValidApplicationId(&artifact.applicationId)
        || !isValidApplicationName(&artifact.applicationName)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("客户端产物元数据标识不匹配：{}", artifact.id),
        ));
    }
    if artifact.sizeBytes == 0
        || artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("客户端生成记录摘要无效：{}", artifact.id),
        ));
    }
    Ok(())
}

/// 删除过期生成记录；APK 由流式响应清理，不允许历史裁剪重新接触秘密产物。
///
/// 运行上下文：历史裁剪和新记录失败回滚共用；不存在视为已清理，其他 I/O 错误阻止状态提交。
async fn removeArtifactRecord(
    packageDirectory: &Path,
    artifact: &ClientPackageArtifact,
) -> Result<(), String> {
    let path = metadataPath(packageDirectory, artifact.id);
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "删除过期客户端生成记录失败（{}）：{error}",
            path.display()
        )),
    }
}

/// 返回产物对应元数据路径；只使用服务生成的 UUID，禁止外部路径片段参与拼接。
fn metadataPath(packageDirectory: &Path, id: Uuid) -> PathBuf {
    packageDirectory.join(format!("{}.json", id.simple()))
}

/// 由任务 UUID 生成稳定 APK 文件名；外部输入不参与路径拼接。
fn packageFileName(id: Uuid) -> String {
    format!("sprak-client-{}.apk", id.simple())
}

/// 由任务 UUID 生成三段随机小写英文包名；每段独立为 3 到 6 个字母且不含固定品牌前缀。
fn randomApplicationId(id: Uuid) -> String {
    let randomBytes = Sha256::digest([b"package".as_slice(), id.as_bytes()].concat());
    let mut cursor = 0usize;
    let mut segments = Vec::with_capacity(3);
    for _ in 0..3 {
        let length = 3 + usize::from(randomBytes[cursor] % 4);
        cursor += 1;
        segments.push(randomAsciiWord(&randomBytes, cursor, length));
        cursor += length;
    }
    segments.join(".")
}

/// 由任务 UUID 生成 3 到 6 位英文软件名；首字母大写、其余小写，保证不会成为全大写文本。
fn randomApplicationName(id: Uuid) -> String {
    let randomBytes = Sha256::digest([b"name".as_slice(), id.as_bytes()].concat());
    let length = 3 + usize::from(randomBytes[0] % 4);
    let mut name = randomAsciiWord(&randomBytes, 1, length);
    name.get_mut(0..1)
        .expect("随机软件名必须非空")
        .make_ascii_uppercase();
    name
}

/// 从摘要的指定窗口生成固定长度小写英文单词；调用方保证窗口不越过 32 字节摘要。
fn randomAsciiWord(randomBytes: &[u8], start: usize, length: usize) -> String {
    randomBytes[start..start + length]
        .iter()
        .map(|byte| char::from(b'a' + byte % 26))
        .collect()
}

/// 返回 Unix 毫秒时间；系统时钟早于纪元时固定为零，不让诊断时间破坏打包主流程。
fn currentTimeMilliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "../../tests/unit/controlApi/clientPackageControlUnitTests.rs"]
mod tests;
