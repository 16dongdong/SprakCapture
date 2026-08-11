//! 二进制入口只负责创建 Tokio 运行时，服务实现位于可复用的 runtime 模块。

#[tokio::main]
async fn main() {
    proxy_backend::runtime::runControlService().await;
}
