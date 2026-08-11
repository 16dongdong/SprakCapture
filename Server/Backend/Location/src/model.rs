use serde::{Deserialize, Serialize};

/// 描述一组可被录制或工具规则命中的协议位置；空字符串等价于通配。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct LocationPattern {
    pub protocol: String,
    pub host: String,
    pub port: String,
    pub path: String,
    pub query: Option<String>,
}

/// 表示数据面已解析完成的实际目标，避免匹配器重复解析 URL。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedLocation {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: String,
    pub display: String,
}

/// 控制 Location 的兼容匹配细节；默认值与产品设计保持一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct LocationMatchOptions {
    pub caseSensitiveHost: bool,
    pub normalizePath: bool,
}

impl Default for LocationMatchOptions {
    /// 使用 host 大小写不敏感、路径忽略多余尾斜杠的默认匹配规则。
    fn default() -> Self {
        Self {
            caseSensitiveHost: false,
            normalizePath: true,
        }
    }
}
