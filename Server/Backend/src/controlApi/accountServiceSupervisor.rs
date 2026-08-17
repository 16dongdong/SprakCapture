//! 管理独立账号服务子进程、匿名管道握手和内部管理请求。
//!
//! 数据库只由 `accountService` 打开；控制进程仅保存公开监听配置和本次运行的内部端点、令牌。
//! 令牌不会进入配置、命令行、环境变量、日志或公开快照。

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rng};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
    time::timeout,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalLoginTicketResponse {
    path: String,
}

/// 账号校验请求只在回环内部接口和进程内存中存在；序列化结果不得进入日志或持久状态。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientCredentialVerificationRequest<'a> {
    username: &'a str,
    password: &'a str,
}

const accountServiceExecutableOverride: &str = "CAPTURE_ACCOUNT_SERVICE_EXECUTABLE";
const webAssetsDirectoryOverride: &str = "CAPTURE_WEB_ASSETS_DIR";
const controlBaseUrlVariable: &str = "CAPTURE_CONTROL_BASE_URL";
const accountServiceDirectoryName: &str = "accountService";
const accountDatabaseFileName: &str = "accounts.db";
const startupTimeout: Duration = Duration::from_secs(10);
const shutdownTimeout: Duration = Duration::from_secs(5);
const internalRequestTimeout: Duration = Duration::from_secs(5);

/// 保存配置文件允许持久化的账号服务监听参数；内部端点和凭据均属于进程局部状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MultiAccountConfiguration {
    pub enabled: bool,
    #[serde(alias = "managementHost")]
    pub remoteHost: String,
    #[serde(alias = "managementPort")]
    pub remotePort: u16,
}

impl Default for MultiAccountConfiguration {
    /// 首次生产运行默认关闭远程入口；开发 Vite 独立提供免认证页面，不扩大后台监听范围。
    fn default() -> Self {
        Self {
            enabled: false,
            remoteHost: "0.0.0.0".to_owned(),
            remotePort: 19_090,
        }
    }
}

impl MultiAccountConfiguration {
    /// 校验公共监听参数并返回实际绑定地址；失败时调用方不得启动子进程或持久化候选。
    pub fn publicAddress(&self) -> Result<SocketAddr, String> {
        if self.remotePort == 0 {
            return Err("远程管理端口不能为 0".to_owned());
        }
        let host = self
            .remoteHost
            .parse::<IpAddr>()
            .map_err(|_| "远程管理监听地址无效".to_owned())?;
        Ok(SocketAddr::new(host, self.remotePort))
    }
}

/// 表示公开快照中的账号服务状态；管理端状态不与代理数据面状态混用。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Faulted,
}

/// 返回设置页需要的脱敏账号服务视图；完整 API Key 和内部连接材料永不进入该结构。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiAccountPublicState {
    pub enabled: bool,
    pub remoteHost: String,
    pub remotePort: u16,
    pub state: AccountServiceState,
    pub apiKeyPrefix: Option<String>,
    pub apiKeyCreatedAt: Option<i64>,
    pub summary: Option<MultiAccountSummary>,
    pub error: Option<String>,
}

/// 主概览只消费聚合在线态和瞬时速率；账号、IP、租约标识及累计流量均不跨越内部接口。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MultiAccountSummary {
    pub onlineAccounts: usize,
    pub activeConnections: usize,
    pub uploadBytesPerSecond: u64,
    pub downloadBytesPerSecond: u64,
}

/// 保存供 SOCKS5 核心原子读取的账号服务实例；三项材料必须来自同一次启动握手。
#[derive(Clone, Debug)]
pub struct AccountServiceEndpoint {
    pub internalEndpoint: String,
    pub internalToken: String,
    pub serviceInstanceId: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartupResponse {
    publicAddress: SocketAddr,
    internalAddress: SocketAddr,
    serviceInstanceId: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupRequest<'a> {
    databasePath: &'a Path,
    publicAddress: SocketAddr,
    internalAddress: SocketAddr,
    internalToken: &'a str,
    controlBaseUrl: &'a str,
    webAssetsDirectory: Option<&'a Path>,
}

struct RunningAccountServiceProcess {
    child: Child,
    stdin: ChildStdin,
    publicAddress: SocketAddr,
    endpoint: AccountServiceEndpoint,
}

struct SupervisorState {
    state: AccountServiceState,
    running: Option<RunningAccountServiceProcess>,
    apiKeyPrefix: Option<String>,
    apiKeyCreatedAt: Option<i64>,
    error: Option<String>,
}

/// 串行拥有子进程生命周期；克隆只共享同一监督状态，不会复制进程句柄。
#[derive(Clone)]
pub struct AccountServiceSupervisor {
    dataDirectory: Arc<PathBuf>,
    state: Arc<Mutex<SupervisorState>>,
    client: reqwest::Client,
}

impl AccountServiceSupervisor {
    /// 创建停止状态监督器；此时不访问数据库、不绑定端口，也不启动后台任务。
    pub fn new(dataDirectory: &Path) -> Result<Self, std::io::Error> {
        let client = reqwest::Client::builder()
            .timeout(internalRequestTimeout)
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Self {
            dataDirectory: Arc::new(dataDirectory.to_path_buf()),
            state: Arc::new(Mutex::new(SupervisorState {
                state: AccountServiceState::Stopped,
                running: None,
                apiKeyPrefix: None,
                apiKeyCreatedAt: None,
                error: None,
            })),
            client,
        })
    }

    /// 启动并完成匿名管道和内部健康检查；任何失败都会回收子进程，禁止发布半初始化端点。
    pub async fn start(
        &self,
        configuration: &MultiAccountConfiguration,
    ) -> Result<AccountServiceEndpoint, String> {
        let publicAddress = if configuration.enabled {
            configuration.publicAddress()?
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
        };
        let mut state = self.state.lock().await;
        if let Some(running) = state.running.as_mut() {
            match running.child.try_wait() {
                Ok(None) => return Ok(running.endpoint.clone()),
                Ok(Some(status)) => {
                    state.running = None;
                    state.state = AccountServiceState::Faulted;
                    state.error = Some(format!("账号服务意外退出：{status}"));
                }
                Err(error) => return Err(format!("读取账号服务进程状态失败：{error}")),
            }
        }
        state.state = AccountServiceState::Starting;
        state.error = None;

        let startResult = self.startProcess(publicAddress).await;
        match startResult {
            Ok(mut running) => {
                let endpoint = running.endpoint.clone();
                let identity = match self.readManagementIdentity(&endpoint).await {
                    Ok(identity) => identity,
                    Err(error) => {
                        let _ = running.child.kill().await;
                        let _ = running.child.wait().await;
                        state.state = AccountServiceState::Faulted;
                        state.error = Some(error.clone());
                        return Err(error);
                    }
                };
                let apiKeyPrefix = identity
                    .get("apiKeyPrefix")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("账号服务管理身份缺少 API Key 指纹");
                let apiKeyCreatedAt = identity
                    .get("apiKeyCreatedAt")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or("账号服务管理身份缺少 Key 生成时间");
                let (apiKeyPrefix, apiKeyCreatedAt) = match (apiKeyPrefix, apiKeyCreatedAt) {
                    (Ok(prefix), Ok(createdAt)) => (prefix, createdAt),
                    _ => {
                        let error = "账号服务管理身份响应缺少必要字段".to_owned();
                        let _ = running.child.kill().await;
                        let _ = running.child.wait().await;
                        state.state = AccountServiceState::Faulted;
                        state.error = Some(error.clone());
                        return Err(error);
                    }
                };
                state.running = Some(running);
                state.state = AccountServiceState::Running;
                state.apiKeyPrefix = Some(apiKeyPrefix.to_owned());
                state.apiKeyCreatedAt = Some(apiKeyCreatedAt);
                Ok(endpoint)
            }
            Err(error) => {
                state.state = AccountServiceState::Faulted;
                state.error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// 有序发送关闭命令并等待 SQLite 刷新；超时或管道失败时强制结束子进程并返回诊断。
    pub async fn stop(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let Some(mut running) = state.running.take() else {
            state.state = AccountServiceState::Stopped;
            state.error = None;
            return Ok(());
        };
        state.state = AccountServiceState::Stopping;
        let commandResult = running.stdin.write_all(b"shutdown\n").await;
        let waitResult = timeout(shutdownTimeout, running.child.wait()).await;
        let result = match (commandResult, waitResult) {
            (Ok(()), Ok(Ok(status))) if status.success() => Ok(()),
            (_, Ok(Ok(status))) => Err(format!("账号服务关闭状态异常：{status}")),
            (_, Ok(Err(error))) => Err(format!("等待账号服务关闭失败：{error}")),
            (_, Err(_)) => {
                running
                    .child
                    .kill()
                    .await
                    .map_err(|error| format!("强制结束账号服务失败：{error}"))?;
                let _ = running.child.wait().await;
                Err("账号服务有序关闭超时，已强制结束".to_owned())
            }
        };
        state.state = if result.is_ok() {
            AccountServiceState::Stopped
        } else {
            AccountServiceState::Faulted
        };
        state.error = result.as_ref().err().cloned();
        result
    }

    /// 返回当前原子端点快照；子进程已退出时立即标记故障，调用方应停止多账号数据面。
    pub async fn endpoint(&self) -> Result<AccountServiceEndpoint, String> {
        let mut state = self.state.lock().await;
        let Some(running) = state.running.as_mut() else {
            return Err("账号服务未运行".to_owned());
        };
        match running.child.try_wait() {
            Ok(None) => Ok(running.endpoint.clone()),
            Ok(Some(status)) => {
                state.running = None;
                state.state = AccountServiceState::Faulted;
                let error = format!("账号服务意外退出：{status}");
                state.error = Some(error.clone());
                Err(error)
            }
            Err(error) => Err(format!("读取账号服务进程状态失败：{error}")),
        }
    }

    /// 同时验证子进程存活和内部 HTTP/SQLite 健康；进程存在但服务卡死同样视为不可用。
    pub async fn health(&self) -> Result<AccountServiceEndpoint, String> {
        let endpoint = self.endpoint().await?;
        self.verifyHealth(&endpoint).await?;
        Ok(endpoint)
    }

    /// 生成脱敏管理快照；0.0.0.0/:: 仅用于绑定，浏览器打开地址规范化为本机回环。
    pub async fn publicState(
        &self,
        configuration: &MultiAccountConfiguration,
    ) -> MultiAccountPublicState {
        let state = self.state.lock().await;
        let serviceState = state.state;
        let apiKeyPrefix = state.apiKeyPrefix.clone();
        let apiKeyCreatedAt = state.apiKeyCreatedAt;
        let serviceError = state.error.clone();
        drop(state);
        let (summary, summaryError) = if serviceState == AccountServiceState::Running {
            match self.readSummary().await {
                Ok(summary) => (Some(summary), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (None, None)
        };
        MultiAccountPublicState {
            enabled: configuration.enabled,
            remoteHost: configuration.remoteHost.clone(),
            remotePort: configuration.remotePort,
            state: serviceState,
            apiKeyPrefix,
            apiKeyCreatedAt,
            summary,
            error: serviceError.or(summaryError),
        }
    }

    /// 签发映射路由的一次性登录路径；只返回受控相对路径，不暴露账号服务真实监听地址。
    ///
    /// 运行上下文：主工作台进入 `/account-management` 前调用，票据由浏览器在同源映射内单次消费。
    /// 失败语义：服务不可用、响应非法或实例在签发期间变化时返回明确错误，调用方不得复用旧路径。
    pub async fn managementSessionPath(&self) -> Result<String, String> {
        let endpoint = self.endpoint().await?;
        let response = self
            .client
            .post(format!(
                "{}/internal/v1/management/session",
                endpoint.internalEndpoint
            ))
            .header("x-account-service-token", &endpoint.internalToken)
            .send()
            .await
            .map_err(|error| format!("签发账号管理会话失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "账号服务拒绝签发管理会话：HTTP {}",
                response.status()
            ));
        }
        let ticket = response
            .json::<LocalLoginTicketResponse>()
            .await
            .map_err(|error| format!("账号管理会话响应无效：{error}"))?;
        if !ticket.path.starts_with("/api/v1/auth/local?ticket=") {
            return Err("账号管理会话返回了非法入口路径".to_owned());
        }
        let currentEndpoint = self.endpoint().await?;
        if currentEndpoint.serviceInstanceId != endpoint.serviceInstanceId {
            return Err("账号服务实例在会话签发期间发生变化".to_owned());
        }
        Ok(format!("/account-management{}", ticket.path))
    }

    /// 返回账号服务公共回环端点供内部映射转发；外部响应和配置永不包含该地址。
    ///
    /// 运行上下文：Backend 的 `/account-management/*` 处理器逐请求读取，确保子进程重启后不使用旧端点。
    /// 失败语义：进程已退出或端点不可用时返回监督器诊断，由映射层转换为 503。
    pub async fn mappedPublicEndpoint(&self) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let running = state.running.as_mut().ok_or("账号服务未运行")?;
        match running.child.try_wait() {
            Ok(None) => {
                let mut mappedAddress = running.publicAddress;
                if mappedAddress.ip().is_unspecified() {
                    mappedAddress.set_ip(if mappedAddress.is_ipv4() {
                        IpAddr::V4(Ipv4Addr::LOCALHOST)
                    } else {
                        IpAddr::V6(Ipv6Addr::LOCALHOST)
                    });
                }
                Ok(format!("http://{mappedAddress}"))
            }
            Ok(Some(status)) => {
                state.running = None;
                state.state = AccountServiceState::Faulted;
                let error = format!("账号服务意外退出：{status}");
                state.error = Some(error.clone());
                Err(error)
            }
            Err(error) => Err(format!("读取账号服务进程状态失败：{error}")),
        }
    }

    /// 读取账号服务聚合统计并只投影概览字段；内部响应中的累计值不会进入主控制面快照。
    async fn readSummary(&self) -> Result<MultiAccountSummary, String> {
        let value = self
            .requestWithoutBody(reqwest::Method::GET, "/internal/v1/statistics")
            .await?;
        serde_json::from_value(value).map_err(|error| format!("账号服务实时摘要无效：{error}"))
    }

    /// 向当前实例发送内部管理请求；令牌只写入请求头，非成功响应映射为脱敏错误。
    pub async fn request<T: Serialize + ?Sized>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &T,
    ) -> Result<serde_json::Value, String> {
        let endpoint = self.endpoint().await?;
        let response = self
            .client
            .request(method, format!("{}{path}", endpoint.internalEndpoint))
            .header("x-account-service-token", endpoint.internalToken)
            .json(body)
            .send()
            .await
            .map_err(|error| format!("账号服务内部请求失败：{error}"))?;
        let status = response.status();
        let value = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("账号服务内部响应无效：{error}"))?;
        if !status.is_success() {
            return Err(format!("账号服务拒绝内部请求：HTTP {status}"));
        }
        if let Some(identity) = value.get("identity") {
            let mut state = self.state.lock().await;
            state.apiKeyPrefix = identity
                .get("apiKeyPrefix")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            state.apiKeyCreatedAt = identity
                .get("apiKeyCreatedAt")
                .and_then(serde_json::Value::as_i64);
        }
        Ok(value)
    }

    /// 向当前账号服务发送无正文内部请求；令牌只进入请求头，响应沿用与写请求相同的脱敏解析边界。
    ///
    /// 运行上下文：控制面读取管理员公开身份时调用；服务未运行、网络失败或非成功状态均返回精确错误文本。
    pub async fn requestWithoutBody(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<serde_json::Value, String> {
        let endpoint = self.endpoint().await?;
        let response = self
            .client
            .request(method, format!("{}{path}", endpoint.internalEndpoint))
            .header("x-account-service-token", endpoint.internalToken)
            .send()
            .await
            .map_err(|error| format!("账号服务内部请求失败：{error}"))?;
        let status = response.status();
        let value = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("账号服务内部响应无效：{error}"))?;
        if !status.is_success() {
            return Err(format!("账号服务拒绝内部请求：HTTP {status}"));
        }
        Ok(value)
    }

    /// 通过账号服务权威存储校验客户端打包凭据，不创建租约或改变在线统计。
    ///
    /// 运行上下文：公开 APK 下载处理器在启动独立打包器前调用；账号密码只进入一次回环 JSON 请求。
    /// 返回 `Ok(false)` 统一表示未知账号、密码错误、禁用或过期，其他状态和传输故障返回脱敏错误。
    pub async fn verifyClientCredentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<bool, String> {
        let endpoint = self.endpoint().await?;
        let response = self
            .client
            .post(format!(
                "{}/internal/v1/accounts/verify",
                endpoint.internalEndpoint
            ))
            .header("x-account-service-token", endpoint.internalToken)
            .json(&ClientCredentialVerificationRequest { username, password })
            .send()
            .await
            .map_err(|error| format!("账号服务凭据校验请求失败：{error}"))?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(true);
        }
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(false);
        }
        Err(format!(
            "账号服务凭据校验返回异常状态：HTTP {}",
            response.status()
        ))
    }

    /// 预检当前是否存在唯一启用规则集；只读取 ID/修订元数据，不复制规则正文到控制进程。
    ///
    /// 运行上下文：凭据校验成功后、启动打包器前调用，保证下载完成的客户端首启即可取得规则。
    /// 返回 `Ok(false)` 表示尚未启用规则，传输、鉴权或存储故障返回脱敏错误供 HTTP 映射 503。
    pub async fn activeClientRuleSetAvailable(&self) -> Result<bool, String> {
        let endpoint = self.endpoint().await?;
        let response = self
            .client
            .get(format!(
                "{}/internal/v1/ruleSets/active",
                endpoint.internalEndpoint
            ))
            .header("x-account-service-token", endpoint.internalToken)
            .send()
            .await
            .map_err(|error| format!("账号服务规则集预检失败：{error}"))?;
        if response.status().is_success() {
            return Ok(true);
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        Err(format!(
            "账号服务规则集预检返回异常状态：HTTP {}",
            response.status()
        ))
    }

    /// 创建子进程并验证握手中的实际端点；启动期间持有监督锁可避免重复进程竞争同一数据库。
    async fn startProcess(
        &self,
        publicAddress: SocketAddr,
    ) -> Result<RunningAccountServiceProcess, String> {
        let databaseDirectory = self.dataDirectory.join(accountServiceDirectoryName);
        std::fs::create_dir_all(&databaseDirectory)
            .map_err(|error| format!("创建账号数据库目录失败：{error}"))?;
        let executable = resolveAccountServiceExecutable()?;
        let mut child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("启动账号服务失败：{error}"))?;
        let mut stdin = child.stdin.take().ok_or("账号服务标准输入管道不可用")?;
        let stdout = child.stdout.take().ok_or("账号服务标准输出管道不可用")?;
        let internalToken = randomInternalToken();
        let controlBaseUrl = resolveControlBaseUrl()?;
        let webAssetsDirectory = if publicAddress.ip().is_loopback() && publicAddress.port() == 0 {
            None
        } else {
            Some(resolveWebAssetsDirectory()?)
        };
        let request = StartupRequest {
            databasePath: &databaseDirectory.join(accountDatabaseFileName),
            publicAddress,
            internalAddress: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            internalToken: &internalToken,
            controlBaseUrl: &controlBaseUrl,
            webAssetsDirectory: webAssetsDirectory.as_deref(),
        };
        let requestLine = serde_json::to_string(&request)
            .map_err(|error| format!("编码账号服务启动配置失败：{error}"))?;
        stdin
            .write_all(format!("{requestLine}\n").as_bytes())
            .await
            .map_err(|error| format!("发送账号服务启动配置失败：{error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("刷新账号服务启动管道失败：{error}"))?;
        let mut lines = BufReader::new(stdout).lines();
        let handshakeLine = timeout(startupTimeout, lines.next_line())
            .await
            .map_err(|_| "等待账号服务启动握手超时".to_owned())?
            .map_err(|error| format!("读取账号服务启动握手失败：{error}"))?
            .ok_or("账号服务未返回启动握手")?;
        let handshake = serde_json::from_str::<StartupResponse>(&handshakeLine)
            .map_err(|error| format!("账号服务启动握手无效：{error}"))?;
        if !isStartupEndpointValid(publicAddress, &handshake) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("账号服务启动握手端点不符合配置".to_owned());
        }
        let endpoint = AccountServiceEndpoint {
            internalEndpoint: format!("http://{}", handshake.internalAddress),
            internalToken,
            serviceInstanceId: handshake.serviceInstanceId,
        };
        self.verifyHealth(&endpoint).await?;
        Ok(RunningAccountServiceProcess {
            child,
            stdin,
            publicAddress: handshake.publicAddress,
            endpoint,
        })
    }

    /// 校验内部健康响应属于本次握手实例；实例不一致表示端点被复用，必须拒绝发布。
    async fn verifyHealth(&self, endpoint: &AccountServiceEndpoint) -> Result<(), String> {
        let response = self
            .client
            .get(format!("{}/internal/v1/health", endpoint.internalEndpoint))
            .header("x-account-service-token", &endpoint.internalToken)
            .send()
            .await
            .map_err(|error| format!("账号服务健康检查失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("账号服务健康检查返回 HTTP {}", response.status()));
        }
        let health = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("账号服务健康响应无效：{error}"))?;
        if health
            .get("serviceInstanceId")
            .and_then(|value| value.as_str())
            != Some(endpoint.serviceInstanceId.as_str())
        {
            return Err("账号服务健康响应实例标识不一致".to_owned());
        }
        Ok(())
    }

    /// 读取脱敏管理身份用于恢复设置页指纹；普通重启不得调用 bootstrap 或改变现有管理员。
    async fn readManagementIdentity(
        &self,
        endpoint: &AccountServiceEndpoint,
    ) -> Result<serde_json::Value, String> {
        let response = self
            .client
            .get(format!(
                "{}/internal/v1/management/identity",
                endpoint.internalEndpoint
            ))
            .header("x-account-service-token", &endpoint.internalToken)
            .send()
            .await
            .map_err(|error| format!("读取账号服务管理身份失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "读取账号服务管理身份返回 HTTP {}",
                response.status()
            ));
        }
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("账号服务管理身份响应无效：{error}"))
    }
}

/// 解析同安装目录中的账号服务二进制；测试可用显式路径注入隔离夹具。
fn resolveAccountServiceExecutable() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(accountServiceExecutableOverride) {
        return Ok(PathBuf::from(path));
    }
    let currentExecutable =
        std::env::current_exe().map_err(|error| format!("定位代理服务可执行文件失败：{error}"))?;
    let fileName = if cfg!(windows) {
        "accountService.exe"
    } else {
        "accountService"
    };
    currentExecutable
        .parent()
        .map(|directory| directory.join(fileName))
        .ok_or_else(|| "代理服务可执行文件没有父目录".to_owned())
}

/// 解析远程 Web 构建目录；桌面安装由资源环境显式注入，独立运行默认读取可执行文件同级 `web`。
///
/// 运行上下文：每次账号服务进程启动前调用，路径通过匿名管道传递且不会进入公开快照。
/// 失败语义：目录或入口文件缺失时返回精确错误，远程服务保持 Faulted 而不提供空白页面。
fn resolveWebAssetsDirectory() -> Result<PathBuf, String> {
    let directory = if let Some(path) = std::env::var_os(webAssetsDirectoryOverride) {
        PathBuf::from(path)
    } else {
        std::env::current_exe()
            .map_err(|error| format!("定位远程 Web 资源失败：{error}"))?
            .parent()
            .ok_or("代理服务可执行文件没有父目录")?
            .join("web")
    };
    if directory.is_dir() && directory.join("index.html").is_file() {
        Ok(directory)
    } else {
        Err(format!("远程 Web 资源目录无效：{}", directory.display()))
    }
}

/// 解析账号服务回连的本机控制地址；默认值与控制进程固定回环端口保持一致。
///
/// 运行上下文：测试和自定义宿主可显式注入隔离端口，账号服务会再次校验协议、主机与根路径。
/// 失败语义：空值直接拒绝，其他格式错误由账号服务启动握手返回完整诊断。
fn resolveControlBaseUrl() -> Result<String, String> {
    let value = std::env::var(controlBaseUrlVariable)
        .unwrap_or_else(|_| "http://127.0.0.1:17890".to_owned());
    if value.is_empty() {
        return Err("本机控制地址不能为空".to_owned());
    }
    Ok(value)
}

/// 生成每次启动独立的 256 位令牌；Base64URL 仅用于 HTTP 头传输，不降低随机熵。
fn randomInternalToken() -> String {
    let mut bytes = [0_u8; 32];
    rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 校验子进程握手端点属于本次启动请求；内部端点始终限制为回环，公开端口仅在显式配置时要求一致。
///
/// 运行上下文：关闭远程管理时公开地址传入端口 0，由操作系统分配内部映射端口；此时若仍比较端口，
/// 监督器会误杀健康子进程并破坏桌面端内嵌账号管理。实例标识为空或固定端口漂移均返回 false。
fn isStartupEndpointValid(requestedPublicAddress: SocketAddr, response: &StartupResponse) -> bool {
    (requestedPublicAddress.port() == 0
        || response.publicAddress.port() == requestedPublicAddress.port())
        && response.internalAddress.ip().is_loopback()
        && !response.serviceInstanceId.is_empty()
}

#[cfg(test)]
#[path = "../../tests/unit/controlApi/accountServiceSupervisorTests.rs"]
mod tests;
