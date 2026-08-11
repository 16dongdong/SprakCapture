//! 提供反向代理与端口转发规则的控制 API，并在配置提交时统一执行监听冲突校验和服务重启。

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::get,
};
use http_proxy_core::{
    AuxiliaryListenerBindings, AuxiliaryListenerConfiguration, PortForwardEntry, ReverseProxyEntry,
};

use super::{
    ApiError, ControlSnapshot, ControlState, ErrorCode, LocalizedApiError, PublicConfiguration,
};
use crate::localization::RequestLocale;

/// 返回规则配置与当前实际绑定；禁用或停止时 bindings 为空，调用方不能从配置端口推断运行状态。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuxiliaryListenerPublicState {
    pub configuration: AuxiliaryListenerConfiguration,
    pub bindings: AuxiliaryListenerBindings,
}

impl ControlState {
    /// 返回辅助监听配置与实际绑定快照；读取不改变服务状态或重新绑定端口。
    async fn auxiliaryListenerState(&self) -> AuxiliaryListenerPublicState {
        let configuration = self.auxiliaryConfiguration.read().await.clone();
        let bindings = self
            .service
            .lock()
            .await
            .runningAuxiliaryListeners
            .as_ref()
            .map(|listeners| listeners.bindings())
            .unwrap_or_default();
        AuxiliaryListenerPublicState {
            configuration,
            bindings,
        }
    }

    /// 用新反向代理集合替换现有集合；端口与服务主监听冲突会在重启前被完整拒绝。
    async fn replaceReverseProxies(
        &self,
        reverseProxies: Vec<ReverseProxyEntry>,
    ) -> Result<ControlSnapshot, ApiError> {
        let mut configuration = self.auxiliaryConfiguration.read().await.clone();
        configuration.reverseProxies = reverseProxies;
        self.replaceAuxiliaryListeners(configuration).await
    }

    /// 用新 TCP 转发集合替换现有集合；配置替换与所有数据面启停串行，提交成功后旧连接已断开。
    async fn replacePortForwards(
        &self,
        portForwards: Vec<PortForwardEntry>,
    ) -> Result<ControlSnapshot, ApiError> {
        let mut configuration = self.auxiliaryConfiguration.read().await.clone();
        configuration.portForwards = portForwards;
        self.replaceAuxiliaryListeners(configuration).await
    }

    /// 原子验证并应用整组辅助监听规则；服务运行时强制停止旧监听器后再以新规则启动，避免端口残留或配置混代。
    async fn replaceAuxiliaryListeners(
        &self,
        configuration: AuxiliaryListenerConfiguration,
    ) -> Result<ControlSnapshot, ApiError> {
        let _operationGuard = self.serviceOperationLock.lock().await;
        let serviceConfiguration = self.configuration.read().await.clone();
        let httpConfiguration = self.httpConfiguration.read().await.clone();
        let processCaptureConfiguration = self.processCaptureConfiguration.read().await.clone();
        validateAuxiliaryListenerConfiguration(
            &configuration,
            &PublicConfiguration::fromInternal(
                &serviceConfiguration,
                &httpConfiguration,
                &processCaptureConfiguration,
            ),
        )?;
        let restartRequired = self.service.lock().await.state != super::ServiceState::Stopped;
        if restartRequired {
            self.stopServiceExclusive().await?;
        }
        if let Err(_error) = self
            .processSelection
            .replaceAuxiliaryListenerConfiguration(configuration.clone())
        {
            if restartRequired {
                let _ = self.startServiceExclusive().await;
            }
            return Err(ApiError::internal(
                ErrorCode::ConfigurationPersistenceFailed,
            ));
        }
        *self.auxiliaryConfiguration.write().await = configuration;
        if restartRequired {
            self.startServiceExclusive().await
        } else {
            Ok(self.snapshot().await)
        }
    }
}

/// 校验辅助监听配置和 SOCKS5/HTTP 正向代理端口不会重叠；任何 unspecified 地址均覆盖同端口全部本机地址。
fn validateAuxiliaryListenerConfiguration(
    configuration: &AuxiliaryListenerConfiguration,
    serviceConfiguration: &PublicConfiguration,
) -> Result<(), ApiError> {
    configuration
        .validate()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration))?;
    let socksAddress = serviceConfiguration
        .listenHost
        .parse::<std::net::IpAddr>()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration))?;
    let httpAddress = serviceConfiguration
        .httpProxy
        .listenHost
        .parse::<std::net::IpAddr>()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration))?;
    let protected = [
        (socksAddress, serviceConfiguration.listenPort, true),
        (
            httpAddress,
            serviceConfiguration.httpProxy.listenPort,
            serviceConfiguration.httpProxy.enabled,
        ),
    ];
    let conflicts = configuration
        .reverseProxies
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| entry.listenAddress())
        .chain(
            configuration
                .portForwards
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.listenAddress()),
        )
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration))?
        .into_iter()
        .any(|address| {
            protected.iter().any(|(host, port, enabled)| {
                *enabled
                    && *port == address.port()
                    && (*host == address.ip()
                        || host.is_unspecified()
                        || address.ip().is_unspecified())
            })
        });
    if conflicts {
        return Err(ApiError::badRequest(
            ErrorCode::ListenerConfigurationConflict,
        ));
    }
    Ok(())
}

/// 将辅助监听规则端点附加到统一控制路由；每个集合独立读取与替换，避免客户端重复发送另一类规则。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route(
            "/api/v1/listeners/reverseProxies",
            get(getReverseProxies).put(updateReverseProxies),
        )
        .route(
            "/api/v1/listeners/portForwards",
            get(getPortForwards).put(updatePortForwards),
        )
}

/// 读取反向代理规则及运行绑定；响应包含同一辅助状态对象以便 UI 显示已绑定端口。
async fn getReverseProxies(
    State(state): State<ControlState>,
) -> Json<AuxiliaryListenerPublicState> {
    Json(state.auxiliaryListenerState().await)
}

/// 读取 TCP 端口转发规则及运行绑定；反向代理规则同时返回以保持两类端口冲突的可见上下文。
async fn getPortForwards(State(state): State<ControlState>) -> Json<AuxiliaryListenerPublicState> {
    Json(state.auxiliaryListenerState().await)
}

/// 替换反向代理集合；JSON 格式或字段错误均映射为稳定配置错误。
async fn updateReverseProxies(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<Vec<ReverseProxyEntry>>, JsonRejection>,
) -> Result<Json<ControlSnapshot>, LocalizedApiError> {
    let Json(reverseProxies) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration).withLocale(locale))?;
    state
        .replaceReverseProxies(reverseProxies)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 替换 TCP 端口转发集合；应用时按完整服务生命周期断开旧连接并重新绑定新端口。
async fn updatePortForwards(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<Vec<PortForwardEntry>>, JsonRejection>,
) -> Result<Json<ControlSnapshot>, LocalizedApiError> {
    let Json(portForwards) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidConfiguration).withLocale(locale))?;
    state
        .replacePortForwards(portForwards)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}
