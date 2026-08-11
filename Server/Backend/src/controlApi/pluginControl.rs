use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, post, put},
};
use plugin_host::{
    ExtensionInstanceSnapshot, ExtensionPackageSnapshot, InvocationTrace, PluginDetails,
    PluginHostError, PluginPlatformConfiguration, PluginSnapshot, PluginUserConfiguration,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::{ApiError, ControlState, ErrorCode, LocalizedApiError, RequestLocale};

const MAXIMUM_PLUGIN_PACKAGE_BYTES: usize = 64 * 1024 * 1024;

/// 接收插件启停意图；未知字段拒绝，防止客户端版本漂移导致静默改变运行态。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginEnabledUpdate {
    enabled: bool,
}

/// 接收完整的插件配置对象；秘密字段可省略，宿主会合并并保留磁盘中的现有值。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginConfigurationUpdate {
    configuration: JsonValue,
}

impl ControlState {
    /// 返回插件宿主的稳定列表快照；不包含插件配置、文件路径或 Native ABI 内部状态。
    fn pluginSnapshots(&self) -> Vec<PluginSnapshot> {
        self.pluginHost.snapshots()
    }

    /// 读取单插件的表单 Schema、非敏感配置和当前生命周期；该详情不进入常规服务快照。
    fn pluginDetails(&self, pluginId: &str) -> Result<PluginDetails, ApiError> {
        self.pluginHost.details(pluginId).map_err(pluginApiError)
    }

    /// 原子切换指定插件；失败时保持原运行态并映射为统一控制错误结构。
    fn setPluginEnabled(&self, pluginId: &str, enabled: bool) -> Result<PluginSnapshot, ApiError> {
        let result = if enabled {
            self.pluginHost.enable(pluginId)
        } else {
            self.pluginHost.disable(pluginId)
        };
        result.map_err(pluginApiError)
    }

    /// 原子写入插件配置；启用插件会在配置生效后重建运行时，现有匹配连接由宿主统一收束。
    fn updatePluginConfiguration(
        &self,
        pluginId: &str,
        configuration: JsonValue,
    ) -> Result<PluginDetails, ApiError> {
        self.pluginHost
            .updateConfiguration(pluginId, configuration)
            .map_err(pluginApiError)
    }

    /// 重新初始化单插件运行时，用于本地开发和故障恢复；宿主确保新回调装载成功后才替换旧实例。
    fn reloadPlugin(&self, pluginId: &str) -> Result<PluginSnapshot, ApiError> {
        self.pluginHost.reload(pluginId).map_err(pluginApiError)
    }

    /// 安装已在压缩包内完成 manifest 校验的本地插件包；安装后保持禁用，必须由显式启用动作加载代码。
    fn installPluginPackage(&self, package: &[u8]) -> Result<PluginSnapshot, ApiError> {
        self.pluginHost
            .installPackage(package)
            .map_err(pluginApiError)
    }

    /// 卸载已停止处理连接的插件包；活动连接存在时由宿主返回冲突，避免强删动态库目录。
    fn uninstallPlugin(&self, pluginId: &str) -> Result<(), ApiError> {
        self.pluginHost.uninstall(pluginId).map_err(pluginApiError)
    }

    /// 返回完整扩展平台的持久用户配置；该文档不含插件包路径和秘密明文。
    fn extensionPlatformConfiguration(&self) -> PluginPlatformConfiguration {
        self.pluginHost.extensionConfiguration().snapshot()
    }

    /// 返回全部完整插件包及其实际运行状态；不把仅写入配置误报为已启动实例。
    fn extensionPackageSnapshots(&self) -> Vec<ExtensionPackageSnapshot> {
        self.pluginHost.extensionManager().snapshots()
    }

    /// 原子写入扩展启停、顺序、覆盖规则、调度参数和配置；磁盘失败时不发布内存快照。
    fn updateExtensionConfiguration(
        &self,
        pluginId: &str,
        configuration: PluginUserConfiguration,
    ) -> Result<PluginPlatformConfiguration, ApiError> {
        self.pluginHost
            .extensionManager()
            .updateConfiguration(pluginId, configuration)
            .map_err(pluginApiError)
    }

    /// 删除一个扩展的宿主配置；插件包本身保持不变，重复删除按幂等成功处理。
    fn removeExtensionConfiguration(
        &self,
        pluginId: &str,
    ) -> Result<PluginPlatformConfiguration, ApiError> {
        self.pluginHost
            .extensionManager()
            .removeConfiguration(pluginId)
            .map_err(pluginApiError)
    }
}

/// 将 Native 宿主错误映射到稳定 HTTP 语义，避免 DLL、路径与系统错误泄露给控制客户端。
fn pluginApiError(error: PluginHostError) -> ApiError {
    match error {
        PluginHostError::NotFound => ApiError::notFound(ErrorCode::PluginNotFound),
        PluginHostError::InvalidConfiguration
        | PluginHostError::Package
        | PluginHostError::PackageTooLarge => {
            ApiError::badRequest(ErrorCode::PluginOperationFailed)
        }
        _ => ApiError::conflict(ErrorCode::PluginOperationFailed),
    }
}

/// 将插件包、配置和生命周期端点挂到统一控制路由；大正文限制仅作用于安装端点，避免放宽普通 JSON 控制请求。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    let packageRoutes = Router::new()
        .route("/api/v1/plugins/packages", post(installPluginPackage))
        .layer(DefaultBodyLimit::max(MAXIMUM_PLUGIN_PACKAGE_BYTES));
    router
        .merge(packageRoutes)
        .route("/api/v1/plugins", get(listPlugins))
        .route(
            "/api/v1/plugins/{pluginId}",
            get(getPluginDetails).delete(uninstallPlugin),
        )
        .route(
            "/api/v1/plugins/{pluginId}/enabled",
            put(updatePluginEnabled),
        )
        .route(
            "/api/v1/plugins/{pluginId}/configuration",
            put(updatePluginConfiguration),
        )
        .route("/api/v1/plugins/{pluginId}/reload", post(reloadPlugin))
        .route("/api/v1/extensions", get(getExtensionPackages))
        .route(
            "/api/v1/extensions/configuration",
            get(getExtensionPlatformConfiguration),
        )
        .route(
            "/api/v1/extensions/configuration/{pluginId}",
            put(updateExtensionPlatformConfiguration).delete(removeExtensionPlatformConfiguration),
        )
        .route(
            "/api/v1/extensions/runtime",
            get(getExtensionRuntimeSnapshots),
        )
        .route(
            "/api/v1/extensions/traces",
            get(getExtensionInvocationTraces).delete(clearExtensionInvocationTraces),
        )
}

/// 返回完整插件包、启用意图、运行态和稳定错误码；顺序按插件 ID 固定。
async fn getExtensionPackages(
    State(state): State<ControlState>,
) -> Json<Vec<ExtensionPackageSnapshot>> {
    Json(state.extensionPackageSnapshots())
}

/// 返回宿主配置文件的当前脱敏快照；调用方可据此编辑启停、执行顺序和模块配置。
async fn getExtensionPlatformConfiguration(
    State(state): State<ControlState>,
) -> Json<PluginPlatformConfiguration> {
    Json(state.extensionPlatformConfiguration())
}

/// 写入单扩展的完整用户意图；未知字段由严格 serde 模型直接拒绝。
async fn updateExtensionPlatformConfiguration(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
    updateResult: Result<Json<PluginUserConfiguration>, JsonRejection>,
) -> Result<Json<PluginPlatformConfiguration>, LocalizedApiError> {
    let Json(update) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::PluginOperationFailed).withLocale(locale))?;
    state
        .updateExtensionConfiguration(&pluginId, update)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 删除单扩展的持久用户意图；返回删除后的完整文档，便于客户端原子替换本地状态。
async fn removeExtensionPlatformConfiguration(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
) -> Result<Json<PluginPlatformConfiguration>, LocalizedApiError> {
    state
        .removeExtensionConfiguration(&pluginId)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回所有已发布隔离运行时实例的状态；不包含第三方配置正文和路径。
async fn getExtensionRuntimeSnapshots(
    State(state): State<ControlState>,
) -> Json<Vec<ExtensionInstanceSnapshot>> {
    Json(state.pluginHost.extensionKernel().snapshots())
}

/// 返回最近的固定预算调用追踪；正文只以输入输出字节数表示。
async fn getExtensionInvocationTraces(
    State(state): State<ControlState>,
) -> Json<Vec<InvocationTrace>> {
    Json(state.pluginHost.extensionKernel().invocationTraces(512))
}

/// 清空调用追踪但不改变插件执行计划、配置和运行状态。
async fn clearExtensionInvocationTraces(State(state): State<ControlState>) -> StatusCode {
    state.pluginHost.extensionKernel().clearInvocationTraces();
    StatusCode::NO_CONTENT
}

/// 返回所有已发现插件，目录扫描错误以插件自身 Failed 状态呈现，不让列表接口整体失败。
async fn listPlugins(State(state): State<ControlState>) -> Json<Vec<PluginSnapshot>> {
    Json(state.pluginSnapshots())
}

/// 读取单插件设置详情；不使用列表快照替代，确保秘密字段脱敏与配置 Schema 始终来自同一宿主读取。
async fn getPluginDetails(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
) -> Result<Json<PluginDetails>, LocalizedApiError> {
    state
        .pluginDetails(&pluginId)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 修改单插件启停状态；启用会加载 Native DLL，禁用会请求关闭该插件处理的活动连接。
async fn updatePluginEnabled(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
    updateResult: Result<Json<PluginEnabledUpdate>, JsonRejection>,
) -> Result<Json<PluginSnapshot>, LocalizedApiError> {
    let Json(update) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::PluginOperationFailed).withLocale(locale))?;
    state
        .setPluginEnabled(&pluginId, update.enabled)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 写入插件声明式配置；请求必须携带对象根值，未知字段和 Schema 约束由宿主在落盘前统一拒绝。
async fn updatePluginConfiguration(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
    updateResult: Result<Json<PluginConfigurationUpdate>, JsonRejection>,
) -> Result<Json<PluginDetails>, LocalizedApiError> {
    let Json(update) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::PluginOperationFailed).withLocale(locale))?;
    state
        .updatePluginConfiguration(&pluginId, update.configuration)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 重建一个插件的 Native 运行时；禁用插件仅返回当前快照，不会隐式改变用户启停意图。
async fn reloadPlugin(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
) -> Result<Json<PluginSnapshot>, LocalizedApiError> {
    state
        .reloadPlugin(&pluginId)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 安装单个 .tplugin.zip；包体仅在本次请求内存中存在，宿主完成校验与隔离解压后立即返回已禁用快照。
async fn installPluginPackage(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    package: Bytes,
) -> Result<(StatusCode, Json<PluginSnapshot>), LocalizedApiError> {
    state
        .installPluginPackage(&package)
        .map(|snapshot| (StatusCode::CREATED, Json(snapshot)))
        .map_err(|error| error.withLocale(locale))
}

/// 卸载一个无活动连接的插件；删除成功后返回 204，客户端必须重读列表而不是保留本地幽灵条目。
async fn uninstallPlugin(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(pluginId): Path<String>,
) -> Result<StatusCode, LocalizedApiError> {
    state
        .uninstallPlugin(&pluginId)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| error.withLocale(locale))
}
