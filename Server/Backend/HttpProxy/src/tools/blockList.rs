use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderValue, StatusCode,
    header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE},
};
use location_core::{LocationPattern, ResolvedLocation};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{
    PipelineContext, PipelineDirective, PipelineTool, SyntheticResponse, ToolId, ToolPhase,
    ToolRegistration,
    tools::{
        ToolError,
        locationScope::{matchesLocations, validateLocations},
        pipelineError,
    },
};

const maximumResponseBodyBytes: usize = 64 * 1024;

/// 定义列表规则的访问语义；关闭模式完全跳过匹配，白名单模式的空列表拒绝全部位置。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockMode {
    #[default]
    Off,
    BlockList,
    AllowList,
}

/// 定义 Block List 的可持久化配置；Location 列表只接受 location-core 支持的规则语义。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BlockListConfiguration {
    pub mode: BlockMode,
    pub locations: Vec<LocationPattern>,
    pub statusCode: u16,
    pub responseBody: String,
    pub closeConnection: bool,
}

impl Default for BlockListConfiguration {
    /// 使用关闭状态和标准 403 响应作为默认值，确保新建工具实例不会意外阻断任意请求。
    fn default() -> Self {
        Self {
            mode: BlockMode::Off,
            locations: Vec::new(),
            statusCode: 403,
            responseBody: String::new(),
            closeConnection: false,
        }
    }
}

impl BlockListConfiguration {
    /// 在控制面写入前校验状态码和 Location，失败时保留运行时的旧配置不变。
    pub fn validate(&self) -> Result<(), ToolError> {
        if !(100..=599).contains(&self.statusCode) {
            return Err(ToolError::InvalidBlockStatusCode);
        }
        // 合成响应正文会在命中规则时完整驻留于请求路径；限制 UTF-8 字节数使配置快照和数据面内存上界一致。
        if self.responseBody.len() > maximumResponseBodyBytes {
            return Err(ToolError::BlockResponseBodyTooLarge);
        }
        validateLocations(&self.locations)
    }
}

/// 交给工具流水线构造的合成响应草稿；closeConnection 由 HTTP 服务层映射为连接关闭语义。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticBlockResponse {
    pub statusCode: u16,
    pub responseBody: String,
    pub closeConnection: bool,
}

/// 表示 Block List 对单个已解析 Location 的决定；阻断时不允许再发起上游连接。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockListDecision {
    /// 当前模式未命中任何规则，后续请求钩子可继续执行且不记录工具痕迹。
    Continue,
    /// 白名单规则命中并明确放行，流水线应记录该访问决策但保持正常出站。
    Applied,
    /// 当前规则要求阻断，流水线必须跳过上游连接并改用配置化合成响应。
    Block(SyntheticBlockResponse),
}

impl BlockListDecision {
    /// 返回是否需要在事务中标记 blocked；流水线据此短路出站并保留可检查的合成响应。
    pub const fn isBlocked(&self) -> bool {
        matches!(self, Self::Block(_))
    }
}

/// 保存可热更新的 Block List 配置；请求路径仅持有短暂读锁并复制小型配置句柄。
#[derive(Clone, Default)]
pub struct BlockListTool {
    configuration: Arc<RwLock<BlockListConfiguration>>,
}

impl BlockListTool {
    /// 使用已校验配置创建工具实例；配置无效时不创建可被注册的实例。
    pub fn new(configuration: BlockListConfiguration) -> Result<Self, ToolError> {
        configuration.validate()?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 返回当前一致性配置快照，供控制 API 和流水线注册信息读取而不泄露内部锁。
    pub fn configuration(&self) -> BlockListConfiguration {
        self.configuration.read().clone()
    }

    /// 原子替换运行时配置；先完成校验，避免请求线程观察到半有效规则集合。
    pub fn replaceConfiguration(
        &self,
        configuration: BlockListConfiguration,
    ) -> Result<(), ToolError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        Ok(())
    }

    /// 对当前请求位置执行访问列表判断；关闭模式不匹配且始终继续，白名单空列表按拒绝全部处理。
    pub fn onRequest(&self, location: &ResolvedLocation) -> Result<BlockListDecision, ToolError> {
        let configuration = self.configuration();
        match configuration.mode {
            BlockMode::Off => Ok(BlockListDecision::Continue),
            BlockMode::BlockList => {
                let matched = blockListMatches(&configuration.locations, location)?;
                Ok(Self::decisionForBlockList(&configuration, matched))
            }
            BlockMode::AllowList => {
                let matched = blockListMatches(&configuration.locations, location)?;
                Ok(Self::decisionForAllowList(&configuration, matched))
            }
        }
    }

    /// 根据黑名单命中状态产生唯一的阻断响应，未命中时不对后续工具产生副作用。
    fn decisionForBlockList(
        configuration: &BlockListConfiguration,
        matched: bool,
    ) -> BlockListDecision {
        if !matched {
            return BlockListDecision::Continue;
        }
        Self::blockedDecision(configuration)
    }

    /// 根据白名单命中状态区分放行痕迹与阻断，空白名单会以未命中语义稳定拒绝全部位置。
    fn decisionForAllowList(
        configuration: &BlockListConfiguration,
        matched: bool,
    ) -> BlockListDecision {
        if matched {
            return BlockListDecision::Applied;
        }
        Self::blockedDecision(configuration)
    }

    /// 集中构造合成阻断响应，确保状态码、正文和连接关闭策略来自同一配置快照。
    fn blockedDecision(configuration: &BlockListConfiguration) -> BlockListDecision {
        BlockListDecision::Block(SyntheticBlockResponse {
            statusCode: configuration.statusCode,
            responseBody: configuration.responseBody.clone(),
            closeConnection: configuration.closeConnection,
        })
    }
}

/// Block List 的空列表表示没有可阻断目标，而 Allow List 通过对该结果取反实现拒绝全部的专属语义。
fn blockListMatches(
    locations: &[LocationPattern],
    location: &ResolvedLocation,
) -> Result<bool, ToolError> {
    if locations.is_empty() {
        return Ok(false);
    }
    matchesLocations(locations, location)
}

#[async_trait]
impl PipelineTool for BlockListTool {
    /// 读取当前配置快照决定是否参与请求槽位，使控制面热更新立即对后续请求生效。
    fn registration(&self) -> ToolRegistration {
        let configuration = self.configuration();
        ToolRegistration::new(
            ToolId::BlockList,
            vec![ToolPhase::Request],
            configuration.mode != BlockMode::Off,
        )
    }

    /// 在出站前根据当前 Location 生成阻断响应；流水线负责写入 blocked 状态、响应钩子和事务终态。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, crate::PipelineError> {
        let decision = BlockListTool::onRequest(self, &context.location)
            .map_err(|error| pipelineError(ToolId::BlockList, error))?;
        let response = match decision {
            BlockListDecision::Continue => return Ok(PipelineDirective::Continue),
            BlockListDecision::Applied => return Ok(PipelineDirective::Applied),
            BlockListDecision::Block(response) => response,
        };
        let status = StatusCode::from_u16(response.statusCode)
            .map_err(|_| pipelineError(ToolId::BlockList, ToolError::InvalidBlockStatusCode))?;
        let body = Bytes::from(response.responseBody);
        let mut synthetic = SyntheticResponse::new(status, body.clone());
        synthetic.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        synthetic.headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&body.len().to_string())
                .expect("正文长度必须可表示为 HTTP 字段值"),
        );
        if response.closeConnection {
            synthetic
                .headers
                .insert(CONNECTION, HeaderValue::from_static("close"));
        }
        Ok(PipelineDirective::Blocked(synthetic))
    }
}
