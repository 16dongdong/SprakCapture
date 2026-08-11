use serde::Serialize;

/// 定义控制面稳定服务状态，序列化值与 Web 判别联合完全一致。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Faulted,
}
