//! 为 HTTPS 上游维护按客户端证书身份隔离的连接池。
//!
//! rustls 的客户端证书解析器在证书请求回调中拿不到目标主机，因此不能用一个全局连接器安全地
//! 实现按主机身份。这里在请求进入连接池前先按 Location 选身份，并以证书 ID 建立独立连接池。

use std::{collections::HashMap, sync::Arc};

use http::{Request, Response};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, Error},
    rt::TokioExecutor,
};
use location_core::ResolvedLocation;
use parking_lot::Mutex;

use crate::{
    bodyStream::ProxyBody,
    connector::{FixedConnectTarget, ProxyConnector},
    ssl::SslMitmManager,
    tools::DnsSpoofingTool,
};

type HttpsClient = Client<HttpsConnector<ProxyConnector>, ProxyBody>;

/// 共享默认身份和每个导入身份的 Hyper 连接池；克隆只复制 Arc，不复制套接字或密钥。
#[derive(Clone)]
pub(crate) struct HttpsUpstreamClients {
    ssl: SslMitmManager,
    dnsSpoofing: Arc<DnsSpoofingTool>,
    outbound: transport_core::OutboundConnector,
    fixedTarget: Option<FixedConnectTarget>,
    clients: Arc<Mutex<HashMap<String, HttpsClient>>>,
}

impl HttpsUpstreamClients {
    /// 创建空的惰性连接池；TLS 配置错误会在首个 HTTPS 请求上形成明确上游失败。
    pub(crate) fn new(
        ssl: SslMitmManager,
        dnsSpoofing: Arc<DnsSpoofingTool>,
        outbound: transport_core::OutboundConnector,
    ) -> Self {
        Self {
            ssl,
            dnsSpoofing,
            outbound,
            fixedTarget: None,
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 创建只对单一透明目标固定建连 IP 的 HTTPS 客户端集合。
    ///
    /// 运行上下文：URI 与 TLS ServerName 继续使用逻辑域名，底层 TCP/二级代理 CONNECT 使用 WinDivert 原始 IP。
    /// 失败语义：证书与连接错误保持原有返回语义；固定目标不会作用到其他 authority。
    pub(crate) fn newWithFixedTarget(
        ssl: SslMitmManager,
        dnsSpoofing: Arc<DnsSpoofingTool>,
        outbound: transport_core::OutboundConnector,
        fixedTarget: FixedConnectTarget,
    ) -> Self {
        Self {
            ssl,
            dnsSpoofing,
            outbound,
            fixedTarget: Some(fixedTarget),
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 按 Location 选择客户端身份并复用对应连接池；同一身份不会跨越不同 TLS 配置。
    pub(crate) async fn request(
        &self,
        location: &ResolvedLocation,
        request: Request<ProxyBody>,
    ) -> Result<Response<Incoming>, Error> {
        let identity = self.ssl.resolveClientCertificate(location);
        let key = identity
            .as_ref()
            .map(|selected| selected.info.id.clone())
            .unwrap_or_else(|| "default".to_owned());
        let client = {
            let mut clients = self.clients.lock();
            if let Some(client) = clients.get(&key) {
                client.clone()
            } else {
                let client = self
                    .build(identity)
                    .expect("启动阶段已经验证系统信任根和导入身份");
                clients.insert(key, client.clone());
                client
            }
        };
        client.request(request).await
    }

    /// 构建 HTTP/1.1 上游连接池；连接超时和 DNS 工具与明文池一致。
    ///
    /// 解密入口会把下游 HTTP/1.x/2/3 统一重建为 HTTP/1.1 请求。如果这里继续公布 `h2`
    /// ALPN，Hyper 会在请求语义已经降级后再次把它升级成 HTTP/2；部分严格网关会因此把
    /// multipart、签名请求或 authority 校验判为无效并返回 400。上游协议与重建后的消息
    /// 版本保持一致，才能保证所有主机使用同一套无损转发规则，而不是维护站点特例。
    fn build(
        &self,
        identity: Option<crate::clientCertificate::ClientCertificateIdentity>,
    ) -> Result<HttpsClient, crate::SslMitmError> {
        let tlsConfiguration = self.ssl.upstreamClientConfigurationForIdentity(identity)?;
        let connector = self.fixedTarget.clone().map_or_else(
            || ProxyConnector::new(self.outbound.clone(), self.dnsSpoofing.clone()),
            |target| {
                ProxyConnector::newWithFixedTarget(
                    self.outbound.clone(),
                    self.dnsSpoofing.clone(),
                    target,
                )
            },
        );
        let httpsConnector = HttpsConnectorBuilder::new()
            .with_tls_config(tlsConfiguration)
            .https_only()
            .enable_http1()
            .wrap_connector(connector);
        Ok(Client::builder(TokioExecutor::new()).build(httpsConnector))
    }
}
