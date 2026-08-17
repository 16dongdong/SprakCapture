use std::{
    collections::HashMap,
    net::IpAddr,
    path::{Path as FileSystemPath, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use capture_core::{
    BodyResponse, MessageSide, RecordingConfiguration, RecordingPageView, RecordingSession,
    RecordingSettingsUpdate, RecordingSnapshot, RecordingState, TransactionDetailRecord,
};
use http_proxy_core::{
    AuxiliaryListenerConfiguration, DnsSpoofingTool, HttpProxyConfig, HttpProxyDependencies,
    HttpRuntimeMetrics, RunningAuxiliaryListeners, SslMitmManager, SslPublicState,
    buildHttpConnectionHandler, startAuxiliaryListeners,
};
use location_core::validateLocationPattern;
use parking_lot::Mutex as SynchronousMutex;
use plugin_host::PluginHost;
use process_capture_core::{
    ProcessCapture, ProcessCaptureConfiguration, ProcessCaptureError, ProcessCaptureSnapshot,
};
use serde::{Deserialize, Serialize};
use socks5_core::interception::{
    PortProtocolHandler, TcpTunnel, TcpTunnelDisposition, TcpTunnelInterceptor,
};
use socks5_core::{
    AccountServiceClientConfig, AddressOverride, AuthenticationMode, CaptureGeneration,
    FusedProxyDependencies, FusedProxyOptions, RunningServer, ServerSnapshot, ServiceMetrics,
    SessionSnapshot, SessionState, Socks5Config, model::currentTimeMilliseconds,
    startFusedProxyServer,
};
use tokio::{
    sync::{Mutex, RwLock, broadcast, watch},
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;
use transport_core::{UpstreamProxyConfiguration, UpstreamProxyProtocol};
use uuid::Uuid;

use crate::localization::{ErrorCode, Locale, MessageParams, RequestLocale, localizeError};
use crate::socksHttpInspection::SocksHttpInspector;
use crate::socksTransactionProjection::SocksTransactionProjector;
use crate::transactionProjection::{TransactionPage, TransactionPageSource, buildTransactionPage};
use crate::transparentRecording::TransparentRecording;

/// 把 HTTP 工具层的 DNS 规则适配到 SOCKS5 核心的最小解析接口，避免两个数据面复制配置。
struct SocksDnsOverride {
    tool: Arc<DnsSpoofingTool>,
    ruleServiceIp: Option<IpAddr>,
}

impl AddressOverride for SocksDnsOverride {
    /// 返回当前热更新快照命中的 IP；未命中时由 SOCKS5 核心继续使用系统 DNS。
    fn resolveIp(&self, host: &str) -> Option<IpAddr> {
        if host.eq_ignore_ascii_case(clientRulesHost) {
            return self.ruleServiceIp;
        }
        self.tool.resolveIp(host)
    }
}

/// APK 仅通过已认证 SOCKS5 访问此保留域名；服务端把它解析到本机规则服务，避免依赖公网 NAT 回流。
const clientRulesHost: &str = "client-rules.internal.invalid";

/// 把普通 HTTP 连接与 WinDivert 透明连接汇入同一个端口，并以流表原目标优先于载荷分类。
struct UnifiedProtocolHandler {
    http: http_proxy_core::HttpConnectionHandler,
    inspector: SocksHttpInspector,
    processCapture: Arc<ProcessCapture>,
    outbound: transport_core::OutboundConnector,
    processSelection: processControl::ProcessSelectionStore,
    transparentRecording: TransparentRecording,
    pluginHost: PluginHost,
}

impl PortProtocolHandler for UnifiedProtocolHandler {
    /// 在读取协议首字节前查询 WinDivert 流表，避免透明二进制流的 `0x05` 被误判为显式 SOCKS5 握手。
    fn claimsConnection(
        &self,
        stream: &tokio::net::TcpStream,
        clientAddress: std::net::SocketAddr,
    ) -> bool {
        stream.local_addr().ok().is_some_and(|local| {
            self.processCapture
                .originalTargetForPeer(local.ip(), clientAddress)
                .is_some()
        })
    }

    /// 命中 WinDivert 流表时按原目标建立隧道，否则交给显式 HTTP 代理状态机。
    fn serve(
        &self,
        stream: tokio::net::TcpStream,
        clientAddress: std::net::SocketAddr,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let originalTarget = stream.local_addr().ok().and_then(|local| {
            self.processCapture
                .originalTargetForPeer(local.ip(), clientAddress)
        });
        let http = self.http.clone();
        let inspector = self.inspector.clone();
        let outbound = self.outbound.clone();
        let clientProcess = originalTarget
            .as_ref()
            .and_then(|target| self.processSelection.processIdentity(target.processId));
        let transparentRecording = self.transparentRecording.clone();
        let pluginHost = self.pluginHost.clone();
        Box::pin(async move {
            let Some(originalTarget) = originalTarget else {
                http.serve(stream, clientAddress, cancellation).await;
                return;
            };
            let targetIp = originalTarget.address.ip();
            let targetPort = originalTarget.address.port();
            // WinDivert 的 SOCKET/FLOW 元数据只保存 IP。Host/SNI 仅用于恢复应用层域名；实际 TCP 路由始终固定到
            // 内核确认的原始 IP，因此 CDN、DNS 轮转或不同解析视图不会再把合法 HTTP/HTTPS 错判成 TCP。
            let targetHost = inspector
                .resolveTransparentHost(&stream, targetIp, &cancellation)
                .await
                .unwrap_or_else(|_| targetIp.to_string());
            let directTarget = targetIp.to_string();
            let processName = clientProcess.as_ref().map(|process| process.name.clone());
            let Ok(remoteStream) = outbound.connect(&directTarget, targetPort).await else {
                return;
            };
            let tunnel = TcpTunnel {
                clientStream: stream,
                remoteStream,
                clientAddress,
                clientProcessName: processName,
                clientProcessId: Some(originalTarget.processId),
                targetHost,
                connectHost: directTarget,
                routePinned: true,
                targetPort,
                cancellation,
                // WinDivert 透明连接不经过 SOCKS 认证，因此不存在可计费账号租约。
                accountLease: None,
            };
            // 透明 Raw/RawTls 过去直接复制套接字，既绕过事务模型又把连接错误静默吞掉；
            // 现在统一交给增量 spool 录制器，并为分类、录制失败保留稳定诊断而不暴露本机路径。
            match inspector.intercept(tunnel).await {
                Ok(TcpTunnelDisposition::Raw {
                    tunnel,
                    applicationProtocol,
                }) => {
                    if let Err(error) = transparentRecording
                        .relayWithDataPlane(*tunnel, applicationProtocol, pluginHost)
                        .await
                    {
                        eprintln!(
                            "透明流连接结束：code=transparentRelayFailed, kind={:?}",
                            error.kind()
                        );
                    }
                }
                Ok(TcpTunnelDisposition::Handled(_) | TcpTunnelDisposition::Failed { .. }) => {}
                Err(error) => {
                    eprintln!(
                        "透明流协议分类失败：code=transparentClassificationFailed, kind={:?}",
                        error.kind()
                    );
                }
            }
        })
    }

    /// 关闭共享 HTTP 处理器并强制中止其派生的 CONNECT/TLS 任务。
    ///
    /// 运行上下文：保留该接口供通用协议处理器契约调用；融合监听的停止路径直接调用
    /// `abortAndWait`，不会等待长响应或隧道自行退出。
    fn shutdown(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>> {
        let http = self.http.clone();
        let inspector = self.inspector.clone();
        let transparentRecording = self.transparentRecording.clone();
        Box::pin(async move {
            tokio::try_join!(
                PortProtocolHandler::shutdown(&http),
                async move {
                    inspector.shutdown().await;
                    Ok::<(), std::io::Error>(())
                },
                async move {
                    transparentRecording.shutdown().await;
                    Ok::<(), std::io::Error>(())
                }
            )?;
            Ok(())
        })
    }

    /// 中止全部协议与录制任务所有者中的残留任务并等待析构，确保停止返回时不泄漏后台连接。
    fn abortAndWait(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let http = self.http.clone();
        let inspector = self.inspector.clone();
        let transparentRecording = self.transparentRecording.clone();
        Box::pin(async move {
            tokio::join!(
                PortProtocolHandler::abortAndWait(&http),
                inspector.abortAndWait(),
                transparentRecording.abortAndWait()
            );
        })
    }
}

const runtimeEventCoalescingInterval: Duration = Duration::from_millis(50);
const runtimeEventDrainTimeout: Duration = Duration::from_secs(1);
const snapshotTransactionLimit: usize = 500;
const maximumTransactionPageSize: usize = 1_000;
const defaultTransactionPageSize: usize = 200;

mod accountRecovery;
mod accountServiceControl;
mod accountServiceMapping;
mod accountServiceSupervisor;
mod applicationBodyDecoder;
mod clientPackageControl;
mod dataDirectory;
mod httpControl;
mod initialization;
mod listenerControl;
mod mapLocalImport;
mod mcpControl;
mod mediaPreviewControl;
mod pluginControl;
mod processControl;
pub(crate) use processControl::ProcessSelectionStore;
mod processIcon;
mod protocolControl;
mod repeatControl;
mod runtimeEventControl;
mod serviceStartup;
mod serviceState;
mod sslControl;
mod stateProjection;
mod toolControl;
mod uiContextControl;
pub use httpControl::{ApiError, EventMessage, createControlRouter};
use httpControl::{
    DecodedBodyResponse, EncodedBodyResponse, LocalizedApiError, RecordingResponse,
    TransactionDetail, TransactionQuery, mapCaptureLookupError, mapCaptureOperationError,
};
use runtimeEventControl::{
    archiveHttpRuntimeMetrics, archiveRuntimeSnapshot, drainRuntimeEventForwarder,
    forwardAdvancedRepeatEvents, forwardBreakpointEvents, forwardHttpMetricEvents,
    forwardPluginEvents, forwardRecordingEvents, forwardRuntimeEvents,
    releaseExitedCaptureGeneration, waitForControlShutdown, waitForServerExit,
};

pub use accountServiceSupervisor::{
    AccountServiceState, MultiAccountPublicState, MultiAccountSummary,
};
use accountServiceSupervisor::{AccountServiceSupervisor, MultiAccountConfiguration};
pub use initialization::ControlInitializationError;
pub use serviceState::ServiceState;
pub use toolControl::ToolsPublicState;

/// 表示可公开的认证模式，响应中只包含模式和用户名而不包含密码。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublicAuthenticationMode {
    None,
    Password,
    Plugin,
}

/// 保存可返回前端的服务配置；任何字段都不携带原始口令。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfiguration {
    pub startServiceOnLaunch: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub authenticationMode: PublicAuthenticationMode,
    pub authenticationUsernames: Vec<String>,
    pub maxConnections: usize,
    pub connectTimeout: f64,
    pub bindTimeout: f64,
    pub idleTimeout: f64,
    pub shutdownTimeout: f64,
    pub readTimeout: f64,
    pub relayBufferSize: usize,
    pub udpBindHost: String,
    pub udpMaxPacketSize: usize,
    pub httpProxy: PublicHttpProxyConfiguration,
    pub upstreamProxy: PublicUpstreamProxyConfiguration,
    pub processCapture: ProcessCaptureConfiguration,
    pub multiAccount: MultiAccountPublicState,
}

/// 返回不含二级代理口令的公开配置；`hasPassword` 仅帮助界面保留现有凭据。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicUpstreamProxyConfiguration {
    pub enabled: bool,
    pub protocol: UpstreamProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub hasPassword: bool,
}

/// 接收二级代理更新；password=null 表示保留当前口令，空字符串表示明确清除。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpstreamProxyUpdate {
    pub enabled: bool,
    pub protocol: UpstreamProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
}

/// 保存 HTTP 正向代理的公开配置；正文预算仅限制录制副本，不限制线上转发。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicHttpProxyConfiguration {
    pub enabled: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub maxConnections: usize,
    pub maxHeaderBytes: usize,
    pub maxCaptureBodyBytes: usize,
    pub connectTimeoutMilliseconds: u64,
    pub requestTimeoutMilliseconds: u64,
    pub headerReadTimeoutMilliseconds: u64,
    pub shutdownTimeoutMilliseconds: u64,
}

/// 保存 HTTP 监听启用状态和已由核心库校验的运行配置。
#[derive(Clone, Debug)]
struct ManagedHttpProxyConfiguration {
    enabled: bool,
    configuration: HttpProxyConfig,
}

impl Default for ManagedHttpProxyConfiguration {
    /// 默认启用本机 HTTP 代理，端口与 HTTP 核心默认值保持一致。
    fn default() -> Self {
        Self {
            enabled: true,
            configuration: HttpProxyConfig::default(),
        }
    }
}

impl PublicHttpProxyConfiguration {
    /// 从内部 HTTP 配置生成不含运行句柄和本机文件位置的公开响应。
    fn fromInternal(configuration: &ManagedHttpProxyConfiguration) -> Self {
        let proxy = &configuration.configuration;
        Self {
            enabled: configuration.enabled,
            listenHost: proxy.listenHost.to_string(),
            listenPort: proxy.listenPort,
            maxConnections: proxy.maxConnections,
            maxHeaderBytes: proxy.maxHeaderBytes,
            maxCaptureBodyBytes: proxy.maxCaptureBodyBytes,
            connectTimeoutMilliseconds: proxy.connectTimeoutMilliseconds,
            requestTimeoutMilliseconds: proxy.requestTimeoutMilliseconds,
            headerReadTimeoutMilliseconds: proxy.headerReadTimeoutMilliseconds,
            shutdownTimeoutMilliseconds: proxy.shutdownTimeoutMilliseconds,
        }
    }
}

/// 表示配置更新中的单个认证账户；口令仅用于生成运行配置，不进入响应或事件。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialsUpdate {
    pub username: String,
    pub password: String,
}

/// 接收 Web 设置页完整配置；未知字段直接拒绝以暴露协议版本漂移。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationUpdate {
    #[serde(default)]
    pub startServiceOnLaunch: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub authenticationMode: PublicAuthenticationMode,
    pub maxConnections: usize,
    pub connectTimeout: f64,
    pub bindTimeout: f64,
    pub idleTimeout: f64,
    pub shutdownTimeout: f64,
    pub readTimeout: f64,
    pub relayBufferSize: usize,
    pub udpBindHost: String,
    pub udpMaxPacketSize: usize,
    #[serde(deserialize_with = "deserializeRequiredCredentials")]
    pub credentials: Option<CredentialsUpdate>,
    pub httpProxy: PublicHttpProxyConfiguration,
    pub upstreamProxy: UpstreamProxyUpdate,
    pub processCapture: ProcessCaptureConfiguration,
    #[serde(default)]
    pub multiAccount: MultiAccountConfiguration,
}

/// 聚合 SOCKS、HTTP、进程捕获与启动偏好，供公开投影和持久化投影共享同一配置边界。
///
/// 该对象只借用已经完成校验的内部状态，不改变任何协议字段；收拢参数可避免调用方交换同类型配置，
/// 并确保以后扩展核心配置时由编译器要求所有投影入口同步更新。
struct ConfigurationProjectionSource<'a> {
    socks5: &'a Socks5Config,
    http: &'a ManagedHttpProxyConfiguration,
    processCapture: &'a ProcessCaptureConfiguration,
    startServiceOnLaunch: bool,
}

/// 反序列化协议中必填但可为 null 的 credentials 字段；字段缺失由 Serde 明确拒绝。
fn deserializeRequiredCredentials<'de, D>(
    deserializer: D,
) -> Result<Option<CredentialsUpdate>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<CredentialsUpdate>::deserialize(deserializer)
}

impl PublicConfiguration {
    /// 从内部配置生成脱敏响应；用户名排序保证快照稳定。
    fn fromInternal(
        source: ConfigurationProjectionSource<'_>,
        multiAccount: MultiAccountPublicState,
    ) -> Self {
        let ConfigurationProjectionSource {
            socks5: configuration,
            http: httpConfiguration,
            processCapture,
            startServiceOnLaunch,
        } = source;
        let mut authenticationUsernames: Vec<String> =
            configuration.users.keys().cloned().collect();
        authenticationUsernames.sort();
        Self {
            startServiceOnLaunch,
            listenHost: configuration.listenHost.to_string(),
            listenPort: configuration.listenPort,
            authenticationMode: match configuration.authenticationMode {
                AuthenticationMode::NoAuth => PublicAuthenticationMode::None,
                AuthenticationMode::UsernamePassword => PublicAuthenticationMode::Password,
                AuthenticationMode::Plugin => PublicAuthenticationMode::Plugin,
                AuthenticationMode::AccountService => PublicAuthenticationMode::Password,
            },
            authenticationUsernames,
            maxConnections: configuration.maxConnections,
            connectTimeout: millisecondsToSeconds(configuration.connectTimeoutMilliseconds),
            bindTimeout: millisecondsToSeconds(configuration.bindTimeoutMilliseconds),
            idleTimeout: millisecondsToSeconds(configuration.idleTimeoutMilliseconds),
            shutdownTimeout: millisecondsToSeconds(configuration.shutdownTimeoutMilliseconds),
            readTimeout: millisecondsToSeconds(configuration.readTimeoutMilliseconds),
            relayBufferSize: configuration.relayBufferSize,
            udpBindHost: configuration.udpBindHost.clone(),
            udpMaxPacketSize: configuration.udpMaxPacketSize,
            httpProxy: PublicHttpProxyConfiguration::fromInternal(httpConfiguration),
            upstreamProxy: PublicUpstreamProxyConfiguration::fromInternal(
                &httpConfiguration.configuration.upstreamProxy,
            ),
            processCapture: processCapture.clone(),
            multiAccount,
        }
    }
}

impl PublicUpstreamProxyConfiguration {
    /// 从内部配置生成脱敏视图，任何响应和事件都不携带二级代理口令。
    fn fromInternal(configuration: &UpstreamProxyConfiguration) -> Self {
        Self {
            enabled: configuration.enabled,
            protocol: configuration.protocol,
            host: configuration.host.clone(),
            port: configuration.port,
            username: configuration.username.clone(),
            hasPassword: !configuration.password.is_empty(),
        }
    }
}

impl ConfigurationUpdate {
    /// 从已验证的内部状态生成可持久化配置；与公开快照不同，该结构保留认证与二级代理口令。
    fn fromInternal(
        source: ConfigurationProjectionSource<'_>,
        multiAccount: MultiAccountConfiguration,
    ) -> Self {
        let ConfigurationProjectionSource {
            socks5: configuration,
            http: httpConfiguration,
            processCapture,
            startServiceOnLaunch,
        } = source;
        let credentials = configuration
            .users
            .iter()
            .next()
            .map(|(username, password)| CredentialsUpdate {
                username: username.clone(),
                password: password.clone(),
            });
        Self {
            startServiceOnLaunch,
            listenHost: configuration.listenHost.to_string(),
            listenPort: configuration.listenPort,
            authenticationMode: match configuration.authenticationMode {
                AuthenticationMode::NoAuth => PublicAuthenticationMode::None,
                AuthenticationMode::UsernamePassword => PublicAuthenticationMode::Password,
                AuthenticationMode::Plugin => PublicAuthenticationMode::Plugin,
                AuthenticationMode::AccountService => PublicAuthenticationMode::Password,
            },
            maxConnections: configuration.maxConnections,
            connectTimeout: millisecondsToSeconds(configuration.connectTimeoutMilliseconds),
            bindTimeout: millisecondsToSeconds(configuration.bindTimeoutMilliseconds),
            idleTimeout: millisecondsToSeconds(configuration.idleTimeoutMilliseconds),
            shutdownTimeout: millisecondsToSeconds(configuration.shutdownTimeoutMilliseconds),
            readTimeout: millisecondsToSeconds(configuration.readTimeoutMilliseconds),
            relayBufferSize: configuration.relayBufferSize,
            udpBindHost: configuration.udpBindHost.clone(),
            udpMaxPacketSize: configuration.udpMaxPacketSize,
            credentials,
            httpProxy: PublicHttpProxyConfiguration::fromInternal(httpConfiguration),
            upstreamProxy: UpstreamProxyUpdate {
                enabled: httpConfiguration.configuration.upstreamProxy.enabled,
                protocol: httpConfiguration.configuration.upstreamProxy.protocol,
                host: httpConfiguration.configuration.upstreamProxy.host.clone(),
                port: httpConfiguration.configuration.upstreamProxy.port,
                username: httpConfiguration
                    .configuration
                    .upstreamProxy
                    .username
                    .clone(),
                password: Some(
                    httpConfiguration
                        .configuration
                        .upstreamProxy
                        .password
                        .clone(),
                ),
            },
            processCapture: processCapture.clone(),
            multiAccount,
        }
    }

    /// 转换并校验公开配置；密码模式必须同时提供非空凭据。
    fn intoInternal(
        self,
        current: &Socks5Config,
        currentHttp: &ManagedHttpProxyConfiguration,
    ) -> Result<
        (
            Socks5Config,
            ManagedHttpProxyConfiguration,
            ProcessCaptureConfiguration,
            MultiAccountConfiguration,
        ),
        ApiError,
    > {
        if self.listenPort == 0 {
            return Err(ApiError::badRequest(ErrorCode::InvalidListenPort));
        }
        let listenHost = self
            .listenHost
            .parse::<IpAddr>()
            .map_err(|_| ApiError::badRequest(ErrorCode::InvalidListenHost))?;
        let (authenticationMode, users) = match (self.authenticationMode, self.credentials) {
            (PublicAuthenticationMode::None, None) => (AuthenticationMode::NoAuth, HashMap::new()),
            (PublicAuthenticationMode::None, Some(_)) => {
                return Err(ApiError::badRequest(
                    ErrorCode::CredentialsForbiddenWithoutAuthentication,
                ));
            }
            (PublicAuthenticationMode::Password, Some(credentials))
                if !credentials.username.is_empty() && !credentials.password.is_empty() =>
            {
                (
                    AuthenticationMode::UsernamePassword,
                    HashMap::from([(credentials.username, credentials.password)]),
                )
            }
            (PublicAuthenticationMode::Password, None)
                if current.authenticationMode == AuthenticationMode::UsernamePassword
                    && !current.users.is_empty() =>
            {
                (AuthenticationMode::UsernamePassword, current.users.clone())
            }
            (PublicAuthenticationMode::Password, _) => {
                return Err(ApiError::badRequest(ErrorCode::CredentialsRequired));
            }
            (PublicAuthenticationMode::Plugin, None) => {
                (AuthenticationMode::Plugin, HashMap::new())
            }
            (PublicAuthenticationMode::Plugin, Some(_)) => {
                return Err(ApiError::badRequest(
                    ErrorCode::CredentialsForbiddenWithoutAuthentication,
                ));
            }
        };
        let configuration = Socks5Config {
            listenHost,
            listenPort: self.listenPort,
            authenticationMode,
            users,
            maxConnections: self.maxConnections,
            connectTimeoutMilliseconds: secondsToMilliseconds(
                "connectTimeout",
                self.connectTimeout,
            )?,
            bindTimeoutMilliseconds: secondsToMilliseconds("bindTimeout", self.bindTimeout)?,
            idleTimeoutMilliseconds: secondsToMilliseconds("idleTimeout", self.idleTimeout)?,
            shutdownTimeoutMilliseconds: secondsToMilliseconds(
                "shutdownTimeout",
                self.shutdownTimeout,
            )?,
            readTimeoutMilliseconds: secondsToMilliseconds("readTimeout", self.readTimeout)?,
            relayBufferSize: self.relayBufferSize,
            udpBindHost: self.udpBindHost,
            udpMaxPacketSize: self.udpMaxPacketSize,
            udpRemoteLimit: current.udpRemoteLimit,
            sessionHistoryLimit: current.sessionHistoryLimit,
        };
        configuration.validate().map_err(|error| {
            ApiError::badRequest(ErrorCode::InvalidConfiguration)
                .withParam("detail", error.to_string())
        })?;
        let mut httpConfiguration = self.httpProxy.intoInternal()?;
        // HTTP 与 SOCKS5 共享唯一接受端口；HTTP 资源设置不再拥有独立监听生命周期。
        httpConfiguration.enabled = true;
        httpConfiguration.configuration.listenHost = configuration.listenHost;
        httpConfiguration.configuration.listenPort = configuration.listenPort;
        httpConfiguration.configuration.upstreamProxy = self
            .upstreamProxy
            .intoInternal(&currentHttp.configuration.upstreamProxy);
        httpConfiguration
            .configuration
            .validate()
            .map_err(|error| {
                ApiError::badRequest(ErrorCode::InvalidHttpProxyConfiguration)
                    .withParam("detail", error.to_string())
            })?;
        let mut processCapture = self.processCapture;
        processCapture.proxyPort = configuration.listenPort;
        processCapture.proxyAddress = configuration.listenHost;
        processCapture
            .validate(std::process::id())
            .map_err(|error| {
                ApiError::badRequest(ErrorCode::InvalidConfiguration)
                    .withParam("detail", error.to_string())
            })?;
        self.multiAccount.publicAddress().map_err(|detail| {
            ApiError::badRequest(ErrorCode::InvalidConfiguration).withParam("detail", detail)
        })?;
        Ok((
            configuration,
            httpConfiguration,
            processCapture,
            self.multiAccount,
        ))
    }
}

impl UpstreamProxyUpdate {
    /// 合并脱敏更新与现有口令；端点和字段边界由 HTTP 核心统一校验。
    fn intoInternal(self, current: &UpstreamProxyConfiguration) -> UpstreamProxyConfiguration {
        UpstreamProxyConfiguration {
            enabled: self.enabled,
            protocol: self.protocol,
            host: self.host,
            port: self.port,
            username: self.username,
            password: self.password.unwrap_or_else(|| current.password.clone()),
        }
    }
}

impl PublicHttpProxyConfiguration {
    /// 解析并校验 HTTP 代理配置；监听地址或资源预算无效时拒绝整次配置替换。
    fn intoInternal(self) -> Result<ManagedHttpProxyConfiguration, ApiError> {
        if self.listenPort == 0 {
            return Err(ApiError::badRequest(
                ErrorCode::InvalidHttpProxyConfiguration,
            ));
        }
        let listenHost = self
            .listenHost
            .parse::<IpAddr>()
            .map_err(|_| ApiError::badRequest(ErrorCode::InvalidHttpProxyConfiguration))?;
        let configuration = HttpProxyConfig {
            listenHost,
            listenPort: self.listenPort,
            maxConnections: self.maxConnections,
            maxHeaderBytes: self.maxHeaderBytes,
            maxCaptureBodyBytes: self.maxCaptureBodyBytes,
            connectTimeoutMilliseconds: self.connectTimeoutMilliseconds,
            requestTimeoutMilliseconds: self.requestTimeoutMilliseconds,
            headerReadTimeoutMilliseconds: self.headerReadTimeoutMilliseconds,
            shutdownTimeoutMilliseconds: self.shutdownTimeoutMilliseconds,
            upstreamProxy: UpstreamProxyConfiguration::default(),
        };
        configuration.validate().map_err(|error| {
            ApiError::badRequest(ErrorCode::InvalidHttpProxyConfiguration)
                .withParam("detail", error.to_string())
        })?;
        Ok(ManagedHttpProxyConfiguration {
            enabled: self.enabled,
            configuration,
        })
    }
}

/// 在写入配置文件前校验录制更新；拒绝重新启用裁剪，并完整验证每条忽略规则。
///
/// 运行上下文：旧版控制契约仍可反序列化 `limits`，但完整抓包要求正文与事务只由用户显式
/// 清空，不得因运行时预算静默缩短或淘汰。参数 `update` 是尚未持久化的候选；失败时返回
/// `invalidRecordingLimits` 或 `invalidRecordingLocation`，配置文件与会话均保持原值。
fn validateRecordingSettings(
    _current: &RecordingSnapshot,
    update: &RecordingSettingsUpdate,
) -> Result<(), ApiError> {
    if update.limits.is_some() {
        return Err(ApiError::badRequest(ErrorCode::InvalidRecordingLimits));
    }
    if let Some(ignoreLocations) = update.ignoreLocations.as_ref() {
        for location in ignoreLocations {
            validateLocationPattern(location).map_err(|error| {
                ApiError::badRequest(ErrorCode::InvalidRecordingLocation)
                    .withParam("detail", error.to_string())
            })?;
        }
    }
    Ok(())
}

/// 把内部毫秒值转换为 Web 使用的秒制浮点数。
fn millisecondsToSeconds(milliseconds: u64) -> f64 {
    milliseconds as f64 / 1_000.0
}

/// 校验正有限秒数并转换为毫秒；溢出或舍入到零均明确拒绝。
fn secondsToMilliseconds(fieldName: &str, seconds: f64) -> Result<u64, ApiError> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(ApiError::badRequest(ErrorCode::TimeoutMustBePositiveFinite)
            .withParam("fieldName", fieldName));
    }
    let milliseconds = seconds * 1_000.0;
    if milliseconds > u64::MAX as f64 {
        return Err(
            ApiError::badRequest(ErrorCode::TimeoutOutOfRange).withParam("fieldName", fieldName)
        );
    }
    let rounded = milliseconds.round() as u64;
    if rounded == 0 {
        return Err(ApiError::badRequest(ErrorCode::TimeoutBelowMillisecond)
            .withParam("fieldName", fieldName));
    }
    Ok(rounded)
}

/// 提供 GET /snapshot 的严格响应结构。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlSnapshot {
    /// 标识当前后台进程；revision 只允许在同一实例标识内比较。
    pub serverInstanceId: String,
    pub revision: u64,
    pub serviceState: ServiceState,
    pub metrics: ServiceMetrics,
    pub sessions: Vec<SessionSnapshot>,
    pub configuration: PublicConfiguration,
    pub processCapture: ProcessCaptureSnapshot,
    pub listeners: ListenerSnapshots,
    pub ssl: SslPublicState,
    pub recording: RecordingSnapshot,
    /// 工具配置与流水线顺序来自与 HTTP 数据面共享的唯一运行时实例；断点草稿通过专用端点读取。
    pub tools: ToolsPublicState,
    /// 快照只携带最近的有界事务摘要页，正文和头字段必须从详情端点按需读取。
    pub transactions: TransactionPage,
    /// 高级重复作业由控制快照携带权威全集，实时事件只负责增量唤醒和版本排序。
    pub advancedRepeats: Vec<repeatControl::AdvancedRepeatJob>,
    /// 插件公开状态包含生命周期和活动连接计数；配置详情仍由按需端点读取，避免进入常驻事件流。
    pub plugins: Vec<plugin_host::PluginSnapshot>,
    /// 内置 MCP 使用独立回环监听，可在不重启代理数据面的情况下热启停。
    pub mcp: mcpControl::McpPublicState,
}

/// 描述单个数据面监听器的配置和实际绑定结果；部分启动失败不会掩盖另一监听器状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerSnapshot {
    pub enabled: bool,
    pub state: ListenerState,
    pub boundEndpoint: Option<String>,
    pub error: Option<ListenerErrorSnapshot>,
}

/// 表示单监听器的实际生命周期，不用 boundEndpoint/null 推断失败与禁用。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ListenerState {
    Disabled,
    Stopped,
    Running,
    Failed,
}

/// 提供监听失败的稳定机器契约；params 不包含请求头、查询、正文或认证材料。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerErrorSnapshot {
    pub code: String,
    pub messageKey: String,
    pub params: MessageParams,
}

/// 聚合 SOCKS5 与 HTTP 正向代理监听状态，作为统一服务生命周期的公开视图。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerSnapshots {
    pub socks5: ListenerSnapshot,
    pub httpProxy: ListenerSnapshot,
}

struct ManagedService {
    state: ServiceState,
    runningServer: Option<RunningServer>,
    captureGeneration: Option<CaptureGeneration>,
    runningAuxiliaryListeners: Option<RunningAuxiliaryListeners>,
    eventForwarder: Option<JoinHandle<()>>,
    httpMetricForwarder: Option<JoinHandle<()>>,
    httpMetrics: Option<HttpRuntimeMetrics>,
    exitMonitor: Option<JoinHandle<()>>,
    udpRecording: Option<crate::udpRecording::UdpRecordingRuntime>,
    socksError: Option<ListenerErrorSnapshot>,
    errorMessage: Option<String>,
    archivedSessions: Vec<SessionSnapshot>,
    archivedMetrics: ServiceMetrics,
}

/// 从生命周期拥有者生成监听器公开状态；配置 enabled 与真实 boundEndpoint 分开表达。
fn listenerSnapshots(
    service: &ManagedService,
    httpConfiguration: &ManagedHttpProxyConfiguration,
) -> ListenerSnapshots {
    ListenerSnapshots {
        socks5: ListenerSnapshot {
            enabled: true,
            state: listenerState(
                true,
                service.runningServer.is_some(),
                service.socksError.is_some(),
            ),
            boundEndpoint: service
                .runningServer
                .as_ref()
                .map(|server| server.boundAddress().to_string()),
            error: service.socksError.clone(),
        },
        httpProxy: ListenerSnapshot {
            enabled: httpConfiguration.enabled,
            state: listenerState(
                httpConfiguration.enabled,
                service.runningServer.is_some(),
                service.socksError.is_some(),
            ),
            boundEndpoint: service
                .runningServer
                .as_ref()
                .map(|server| server.boundAddress().to_string()),
            error: service.socksError.clone(),
        },
    }
}

/// 按 enabled、实际句柄和错误优先级生成单监听状态。
fn listenerState(enabled: bool, running: bool, failed: bool) -> ListenerState {
    if !enabled {
        ListenerState::Disabled
    } else if running {
        ListenerState::Running
    } else if failed {
        ListenerState::Failed
    } else {
        ListenerState::Stopped
    }
}

/// 将监听器错误渲染为内部启动诊断；公开契约只暴露稳定 messageKey 和结构化参数。
fn listenerErrorMessage(error: &ListenerErrorSnapshot) -> String {
    if error.code.starts_with("httpProxy") {
        localizeError(
            ErrorCode::HttpProxyListenerFailed,
            Locale::En,
            &error.params,
        )
    } else {
        localizeError(ErrorCode::ServiceStartFailed, Locale::En, &error.params)
    }
}

/// 创建不携带底层诊断和请求材料的监听错误。
fn listenerError(code: &str, messageKey: &str) -> ListenerErrorSnapshot {
    ListenerErrorSnapshot {
        code: code.to_owned(),
        messageKey: messageKey.to_owned(),
        params: MessageParams::new(),
    }
}

/// 合并监听级诊断用于全部监听启动失败时的控制错误；每条信息都来自已脱敏的数据面错误。
fn aggregateListenerErrors(service: &ManagedService) -> Option<String> {
    let errors = [service
        .socksError
        .as_ref()
        .map(|error| format!("[socks5] {}", listenerErrorMessage(error)))]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    (!errors.is_empty()).then(|| errors.join("; "))
}

/// 合并历史与当前运行指标；历史活动连接恒为零，当前活动连接保持实时值。
fn combineMetrics(
    archivedMetrics: &ServiceMetrics,
    currentMetrics: &ServiceMetrics,
) -> ServiceMetrics {
    ServiceMetrics {
        acceptedConnections: archivedMetrics
            .acceptedConnections
            .saturating_add(currentMetrics.acceptedConnections),
        activeConnections: currentMetrics.activeConnections,
        failedConnections: archivedMetrics
            .failedConnections
            .saturating_add(currentMetrics.failedConnections),
        bytesUp: archivedMetrics
            .bytesUp
            .saturating_add(currentMetrics.bytesUp),
        bytesDown: archivedMetrics
            .bytesDown
            .saturating_add(currentMetrics.bytesDown),
        udpPacketsUp: archivedMetrics
            .udpPacketsUp
            .saturating_add(currentMetrics.udpPacketsUp),
        udpPacketsDown: archivedMetrics
            .udpPacketsDown
            .saturating_add(currentMetrics.udpPacketsDown),
        droppedUdpPackets: archivedMetrics
            .droppedUdpPackets
            .saturating_add(currentMetrics.droppedUdpPackets),
    }
}

/// 合并同一服务周期并发运行的 SOCKS 与 HTTP 账本；两个数据面都可能持有活动连接，因此 active 必须相加。
fn combineConcurrentMetrics(left: &ServiceMetrics, right: &ServiceMetrics) -> ServiceMetrics {
    let mut combined = combineMetrics(left, right);
    combined.activeConnections = left
        .activeConnections
        .saturating_add(right.activeConnections);
    combined
}

/// 合并跨服务周期会话并只保留最新关闭历史；活动会话不参与历史上限裁剪。
fn combineSessions(
    archivedSessions: &[SessionSnapshot],
    currentSessions: &[SessionSnapshot],
    historyLimit: usize,
) -> Vec<SessionSnapshot> {
    let mut sessions = Vec::with_capacity(archivedSessions.len() + currentSessions.len());
    sessions.extend_from_slice(archivedSessions);
    sessions.extend_from_slice(currentSessions);
    sessions.sort_by_key(|session| std::cmp::Reverse(session.createdAtMilliseconds));
    let mut closedCount = 0_usize;
    sessions.retain(|session| {
        if !matches!(session.state, SessionState::Closed | SessionState::Failed) {
            return true;
        }
        closedCount += 1;
        closedCount <= historyLimit
    });
    sessions
}

/// 保存 HTTP 处理器共享状态；revision 为所有快照和增量事件提供全局顺序。
#[derive(Clone)]
pub struct ControlState {
    dataDirectory: Arc<PathBuf>,
    serverInstanceId: Arc<str>,
    configuration: Arc<RwLock<Socks5Config>>,
    httpConfiguration: Arc<RwLock<ManagedHttpProxyConfiguration>>,
    processCaptureConfiguration: Arc<RwLock<ProcessCaptureConfiguration>>,
    multiAccountConfiguration: Arc<RwLock<MultiAccountConfiguration>>,
    accountService: AccountServiceSupervisor,
    clientPackages: clientPackageControl::ClientPackageManager,
    startServiceOnLaunch: Arc<AtomicBool>,
    processSelection: processControl::ProcessSelectionStore,
    auxiliaryConfiguration: Arc<RwLock<AuxiliaryListenerConfiguration>>,
    ssl: SslMitmManager,
    recording: RecordingSession,
    mediaPreviewLeaseBudget: mediaPreviewControl::MediaPreviewLeaseBudget,
    tools: toolControl::ToolRuntime,
    protocols: protocolControl::ProtocolRuntime,
    pluginHost: PluginHost,
    repeatRuntime: repeatControl::RepeatRuntime,
    mcp: mcpControl::McpManager,
    uiContexts: uiContextControl::UiContextRegistry,
    processCapture: Arc<ProcessCapture>,
    serviceOperationLock: Arc<Mutex<()>>,
    /// 记录用户对代理数据面的持续运行意图；账号服务故障恢复只能读取该意图，不能凭故障前快照擅自重启。
    serviceRunIntent: Arc<AtomicBool>,
    /// 标识多账号配置代际；监督任务在取得生命周期锁后必须重新核对，防止用等待锁前的旧端口启动子进程。
    multiAccountGeneration: Arc<AtomicU64>,
    service: Arc<Mutex<ManagedService>>,
    revision: Arc<AtomicU64>,
    /// 仅跟踪会改变完整快照结构的低频控制状态；高频指标事件不推进它，避免快照持续重试饥饿。
    projectionGeneration: Arc<AtomicU64>,
    /// 配置替换期间阻止公开快照观察 staged 服务与旧配置的混合窗口。
    configurationTransactionSender: watch::Sender<bool>,
    eventPublishLock: Arc<SynchronousMutex<()>>,
    eventSender: broadcast::Sender<EventMessage>,
    capturePublishLock: Arc<Mutex<()>>,
    recordingUpdateLock: Arc<Mutex<()>>,
    udpRecordingCoordination: Arc<crate::udpRecording::UdpRecordingCoordination>,
    publishedCaptureRevision: Arc<AtomicU64>,
    shutdownSender: watch::Sender<bool>,
}

impl ControlState {
    /// 创建停止状态控制器、单一录制会话和当前用户唯一根 CA；任一初始化失败都会阻止监听。
    pub async fn new() -> Result<Self, ControlInitializationError> {
        let dataDirectory = initialization::defaultDataDirectory()?;
        Self::newWithDataDirectory(&dataDirectory).await
    }

    /// 使用显式数据根目录加载统一配置并创建控制器；证书、映射规则和录制 spill 均归属同一根目录。
    ///
    /// 参数 `dataDirectory` 是当前控制进程唯一数据根；配置格式、规则语义或受控目录初始化失败会
    /// 阻止启动。录制偏好必须在创建会话前恢复，避免启动瞬间错误接纳本应暂停或忽略的事务。
    pub async fn newWithDataDirectory(
        dataDirectory: &FileSystemPath,
    ) -> Result<Self, ControlInitializationError> {
        let (eventSender, _) = broadcast::channel(2_048);
        let (shutdownSender, _) = watch::channel(false);
        let processSelection = processControl::ProcessSelectionStore::load(dataDirectory)?;
        let mcp = mcpControl::McpManager::new(processSelection.mcpConfiguration()).await;
        let recordingConfiguration = processSelection.recordingConfiguration();
        let toolsConfiguration = processSelection.toolsConfiguration();
        let recording = RecordingSession::new(RecordingConfiguration {
            ignoreLocations: recordingConfiguration.ignoreLocations,
            recordTunnelMetadata: recordingConfiguration.recordTunnelMetadata,
            spillDirectory: dataDirectory.join("capture"),
            ..RecordingConfiguration::default()
        })
        .await?;
        if recordingConfiguration.state == RecordingState::Paused {
            // 暂停是用户偏好而不是旧会话运行数据；新会话完成资源初始化后再恢复暂停，
            // 可确保 spill 目录错误仍在启动阶段明确暴露，而不是生成半初始化控制状态。
            recording.pauseRecording().await?;
        }
        let certificateDirectory = initialization::certificateDirectory(dataDirectory);
        let ssl = SslMitmManager::load(&certificateDirectory)?;
        ssl.updateConfiguration(processSelection.sslConfiguration())?;
        let pluginHost = PluginHost::new(dataDirectory.join("plugins"))?;
        let mappingRoot = dataDirectory.join("mappings");
        std::fs::create_dir_all(&mappingRoot)?;
        let tools = toolControl::ToolRuntime::new(
            &mappingRoot,
            recording.clone(),
            pluginHost.packetFilters(),
            toolsConfiguration,
        )
        .map_err(|detail| ControlInitializationError::ToolConfiguration { detail })?;
        let protocols = protocolControl::ProtocolRuntime::new(
            dataDirectory,
            processSelection.protocolConfiguration(),
        )
        .await
        .map_err(ControlInitializationError::ProtocolDescriptorDirectory)?;
        let defaultConfiguration = Socks5Config::default();
        let defaultHttpConfiguration = ManagedHttpProxyConfiguration::default();
        let savedServiceConfiguration = processSelection.serviceConfiguration();
        let startServiceOnLaunch = savedServiceConfiguration
            .as_ref()
            .is_some_and(|configuration| configuration.startServiceOnLaunch);
        let (initialConfiguration, initialHttpConfiguration, _, initialMultiAccountConfiguration) =
            savedServiceConfiguration
                .map(|saved| saved.intoInternal(&defaultConfiguration, &defaultHttpConfiguration))
                .transpose()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "保存的服务配置无效")
                })?
                .unwrap_or((
                    defaultConfiguration,
                    defaultHttpConfiguration,
                    ProcessCaptureConfiguration::default(),
                    MultiAccountConfiguration::default(),
                ));
        let initialProcessCaptureConfiguration =
            processSelection.runtimeConfiguration(initialConfiguration.listenPort);
        // 首次运行也生成完整配置文件；后续启动会从同一文件恢复，而不是依赖仅存在于内存的默认值。
        processSelection.replaceServiceConfiguration(ConfigurationUpdate::fromInternal(
            ConfigurationProjectionSource {
                socks5: &initialConfiguration,
                http: &initialHttpConfiguration,
                processCapture: &initialProcessCaptureConfiguration,
                startServiceOnLaunch,
            },
            initialMultiAccountConfiguration.clone(),
        ))?;
        let accountService = AccountServiceSupervisor::new(dataDirectory)?;
        let clientPackages = clientPackageControl::ClientPackageManager::load(dataDirectory)?;
        // 账号服务同时拥有 SOCKS5 账号数据库与远程 Web 身份；即使远程监听关闭也必须在回环随机端口运行。
        // 启动失败状态保留在公开快照，控制接口仍可用于修正资源或监听配置。
        let _ = accountService
            .start(&initialMultiAccountConfiguration)
            .await;
        let processCapture = Arc::new(ProcessCapture::new());
        processCapture.setUdpDatagramProcessor(Some(Arc::new(
            crate::packetDataPlane::UnifiedPacketFilterProcessor::new(pluginHost.clone()),
        )));
        let serverInstanceId: Arc<str> = Arc::from(Uuid::new_v4().to_string());
        let udpRecordingCoordination = crate::udpRecording::UdpRecordingCoordination::load(
            dataDirectory,
            Arc::clone(&serverInstanceId),
        )?;
        let initialAuxiliaryConfiguration = processSelection.auxiliaryListenerConfiguration();
        let state = Self {
            dataDirectory: Arc::new(dataDirectory.to_path_buf()),
            // UUID 在每次控制进程构造时生成一次，克隆 ControlState 只共享同一代际标识。
            serverInstanceId: Arc::clone(&serverInstanceId),
            configuration: Arc::new(RwLock::new(initialConfiguration)),
            httpConfiguration: Arc::new(RwLock::new(initialHttpConfiguration)),
            processCaptureConfiguration: Arc::new(RwLock::new(initialProcessCaptureConfiguration)),
            multiAccountConfiguration: Arc::new(RwLock::new(initialMultiAccountConfiguration)),
            accountService,
            clientPackages,
            startServiceOnLaunch: Arc::new(AtomicBool::new(startServiceOnLaunch)),
            processSelection,
            auxiliaryConfiguration: Arc::new(RwLock::new(initialAuxiliaryConfiguration)),
            ssl,
            recording: recording.clone(),
            mediaPreviewLeaseBudget: mediaPreviewControl::MediaPreviewLeaseBudget::default(),
            tools,
            protocols,
            pluginHost,
            repeatRuntime: repeatControl::RepeatRuntime::default(),
            mcp,
            uiContexts: uiContextControl::UiContextRegistry::default(),
            processCapture,
            // 启停与配置重启共享同一把操作锁，禁止新监听器在旧配置停机窗口抢占生命周期。
            serviceOperationLock: Arc::new(Mutex::new(())),
            serviceRunIntent: Arc::new(AtomicBool::new(false)),
            multiAccountGeneration: Arc::new(AtomicU64::new(0)),
            service: Arc::new(Mutex::new(ManagedService {
                state: ServiceState::Stopped,
                runningServer: None,
                captureGeneration: None,
                runningAuxiliaryListeners: None,
                eventForwarder: None,
                httpMetricForwarder: None,
                httpMetrics: None,
                exitMonitor: None,
                udpRecording: None,
                socksError: None,
                errorMessage: None,
                archivedSessions: Vec::new(),
                archivedMetrics: ServiceMetrics::default(),
            })),
            revision: Arc::new(AtomicU64::new(0)),
            projectionGeneration: Arc::new(AtomicU64::new(0)),
            configurationTransactionSender: watch::channel(false).0,
            eventPublishLock: Arc::new(SynchronousMutex::new(())),
            eventSender,
            capturePublishLock: Arc::new(Mutex::new(())),
            recordingUpdateLock: Arc::new(Mutex::new(())),
            udpRecordingCoordination,
            publishedCaptureRevision: Arc::new(AtomicU64::new(0)),
            shutdownSender,
        };
        let accountServiceMonitorState = state.clone();
        tokio::spawn(async move {
            accountServiceMonitorState.monitorAccountService().await;
        });
        let forwardingState = state.clone();
        let changes = recording.subscribeChanges();
        tokio::spawn(async move {
            forwardRecordingEvents(forwardingState, changes).await;
        });
        let breakpointForwardingState = state.clone();
        let breakpointChanges = state.tools.subscribeSuspendedChanges();
        tokio::spawn(async move {
            forwardBreakpointEvents(breakpointForwardingState, breakpointChanges).await;
        });
        let advancedRepeatState = state.clone();
        let advancedRepeatChanges = state.repeatRuntime.subscribeChanges();
        tokio::spawn(async move {
            forwardAdvancedRepeatEvents(advancedRepeatState, advancedRepeatChanges).await;
        });
        let pluginState = state.clone();
        let pluginChanges = state.pluginHost.subscribeChanges();
        tokio::spawn(async move {
            forwardPluginEvents(pluginState, pluginChanges).await;
        });
        let processSynchronizationState = state.clone();
        tokio::spawn(async move {
            processControl::synchronizeSelectedProcessIds(processSynchronizationState).await;
        });
        Ok(state)
    }

    /// 删除已结束会话记录并返回最新快照；活动会话继续保留。
    pub async fn clearSessions(&self) -> ControlSnapshot {
        let mut service = self.service.lock().await;
        if let Some(server) = service.runningServer.as_ref() {
            server.clearClosedSessions();
        }
        service.archivedSessions.clear();
        drop(service);
        self.publishRuntimeViews().await;
        self.snapshot().await
    }

    /// 返回当前录制会话状态；该结构不包含事务头、正文或 spill 路径。
    async fn recordingSnapshot(&self) -> Result<RecordingSnapshot, ApiError> {
        self.recording
            .snapshot()
            .await
            .map_err(mapCaptureOperationError)
    }

    /// 用 revision 前后校验返回录制状态，避免并发控制事件产生“新 revision + 旧字段”。
    async fn recordingResponse(&self) -> Result<RecordingResponse, ApiError> {
        let revision = self.currentRevision();
        let recording = self.recordingSnapshot().await?;
        Ok(RecordingResponse {
            serverInstanceId: self.serverInstanceId.to_string(),
            revision,
            recording,
        })
    }

    /// 原子持久化并应用录制状态和过滤偏好，随后发布权威录制与事务视图。
    ///
    /// 运行上下文：`recordingUpdateLock` 同时串行化 clear 与 UDP 投影；配置候选先完成语义校验和
    /// 原子写盘，再进入不会因字段校验失败的会话提交，避免接口成功但重启恢复旧设置。
    /// 参数 `update` 是部分更新；失败时返回精确校验、持久化或录制操作错误。
    async fn updateRecording(
        &self,
        update: RecordingSettingsUpdate,
    ) -> Result<RecordingResponse, ApiError> {
        let _updateGuard = self.recordingUpdateLock.lock().await;
        let current = self.recordingSnapshot().await?;
        validateRecordingSettings(&current, &update)?;
        let mut persisted = processControl::PersistedRecordingConfiguration::fromSnapshot(&current);
        if let Some(state) = update.state {
            persisted.state = state;
        }
        if let Some(ignoreLocations) = update.ignoreLocations.as_ref() {
            persisted.ignoreLocations.clone_from(ignoreLocations);
        }
        if let Some(recordTunnelMetadata) = update.recordTunnelMetadata {
            persisted.recordTunnelMetadata = recordTunnelMetadata;
        }
        self.processSelection
            .replaceRecordingConfiguration(persisted)
            .map_err(|_| ApiError::internal(ErrorCode::ConfigurationPersistenceFailed))?;
        self.recording
            .updateSettings(update)
            .await
            .map_err(mapCaptureOperationError)?;
        self.publishRecordingViews().await?;
        self.recordingResponse().await
    }

    /// 清空事务元数据、头与正文引用；累计 SOCKS 指标和会话历史不受影响。
    async fn clearRecording(&self) -> Result<RecordingResponse, ApiError> {
        let _updateGuard = self.recordingUpdateLock.lock().await;
        self.udpRecordingCoordination
            .advanceAndPersist(&self.dataDirectory)
            .map_err(|error| {
                ApiError::internal(ErrorCode::RecordingOperationFailed)
                    .withParam("detail", error.to_string())
            })?;
        let clearResult = self.recording.clearSession().await;
        // clearSession 在公开集合已经置空后仍可能因 spill 物理清理失败返回错误。UDP 水位已在
        // 同一串行边界持久推进，SOCKS 投影代际也必须推进，否则旧积压会在错误返回后重新出现。
        {
            let service = self.service.lock().await;
            if let Some(server) = service.runningServer.as_ref() {
                // Capture 成功清空后再推进数据面代际；同一串行锁保证在途投影要么先被清空，
                // 要么在解锁后看见新水位并拒绝旧队列事件。清理失败不会破坏原代际。
                server.clearCapturedBytes();
            } else if let Some(captureGeneration) = service.captureGeneration.as_ref() {
                // stop 已 take 走 RunningServer 但投影队列可能仍在排空；保留的代际句柄
                // 使并发 clear 仍能废止所有停止前事件。
                captureGeneration.advance();
            }
        }
        if let Err(error) = clearResult {
            self.publishRecordingViews().await?;
            return Err(mapCaptureOperationError(error));
        }
        self.publishRecordingViews().await?;
        self.recordingResponse().await
    }

    /// 返回有界分页事务摘要；未指定 offset 时默认选择最新一页并保持 sequence 升序。
    async fn transactionPage(&self, query: TransactionQuery) -> Result<TransactionPage, ApiError> {
        let limit = query.limit.unwrap_or(defaultTransactionPageSize);
        if limit == 0
            || limit > maximumTransactionPageSize
            || query.offset.is_some_and(|offset| offset > 0) && query.collectionToken.is_none()
            || query
                .collectionToken
                .as_ref()
                .is_some_and(|token| token.len() > 128)
        {
            return Err(ApiError::badRequest(ErrorCode::InvalidTransactionsQuery));
        }
        let revision = self.currentRevision();
        let RecordingPageView {
            recording,
            collectionToken,
            total,
            offset,
            transactions,
        } = self
            .recording
            .pageView(query.offset, limit, query.collectionToken.as_deref())
            .await
            .map_err(mapCaptureOperationError)?;
        if offset > total {
            return Err(ApiError::badRequest(ErrorCode::InvalidTransactionsQuery));
        }
        Ok(buildTransactionPage(TransactionPageSource {
            revision,
            recordingSessionId: recording.recordingSessionId,
            collectionToken,
            total,
            transactions,
            offset,
            limit,
            preferLatest: query.offset.is_none(),
        }))
    }

    /// 返回单条事务摘要、两侧头与可选正文元信息；正文实际字节不在详情响应中复制。
    async fn transactionDetail(&self, transactionId: &str) -> Result<TransactionDetail, ApiError> {
        let revision = self.currentRevision();
        let TransactionDetailRecord {
            transaction,
            requestHeaders,
            responseHeaders,
            requestBody,
            responseBody,
            requestPackets,
            responsePackets,
        } = self
            .recording
            .getTransactionDetail(transactionId)
            .await
            .map_err(mapCaptureLookupError)?;
        Ok(TransactionDetail {
            revision,
            transaction,
            requestHeaders,
            responseHeaders,
            requestBody,
            responseBody,
            requestPackets,
            responsePackets,
        })
    }

    /// 按需读取单侧正文并生成原始与可选自动解码视图；事务或正文被清空后返回稳定 404。
    ///
    /// 原始字节始终独立编码返回，应用层识别失败只令 `decoded` 为空，不改变 Capture 中的正文。
    async fn transactionBody(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<EncodedBodyResponse, ApiError> {
        let revision = self.currentRevision();
        let BodyResponse { meta, bytes } = self
            .recording
            .getBody(transactionId, side)
            .await
            .map_err(mapCaptureLookupError)?;
        let decoded = applicationBodyDecoder::decodeApplicationBody(
            side,
            &meta.contentType,
            &meta.encoding,
            bytes.as_ref(),
        )
        .map(|decoded| DecodedBodyResponse {
            algorithm: decoded.algorithm,
            contentType: decoded.contentType,
            decodedBytes: decoded.bytes.len(),
            base64: base64Standard.encode(decoded.bytes),
        });
        Ok(EncodedBodyResponse {
            revision,
            meta,
            base64: base64Standard.encode(bytes),
            decoded,
        })
    }
}
