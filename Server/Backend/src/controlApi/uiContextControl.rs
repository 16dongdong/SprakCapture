//! 保存多个前端窗口的短生命周期界面上下文，并通过控制 API 提供只读聚合视图。
//!
//! 界面上下文只描述页面、视图和稳定资源标识，不保存正文、凭据或表单输入。它不属于业务配置，
//! 因此不推进控制面 revision，也不持久化；客户端心跳停止后会自动过期。

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::get,
};
use serde::{Deserialize, Serialize};
use socks5_core::model::currentTimeMilliseconds;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::{ApiError, ControlState, LocalizedApiError};
use crate::localization::{ErrorCode, RequestLocale};

const maximumUiContexts: usize = 32;
const maximumSelectionIds: usize = 64;
const maximumIdentifierBytes: usize = 128;
const maximumQualifierBytes: usize = 64;
const uiContextLifetimeMilliseconds: u64 = 20_000;

/// 区分主工作台、悬浮窗与独立窗口；MCP 可据此选择最符合用户当前操作的界面。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiWindowKind {
    Main,
    Floating,
    Independent,
}

/// 定义允许上报的稳定页面集合，避免把任意 URL、查询参数或秘密数据带入 MCP 结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiPage {
    Overview,
    Connections,
    AccountManagement,
    Settings,
    Plugins,
    Floating,
    Dialog,
}

/// 标识当前选择属于哪类领域对象；具体内容仍由已有 MCP 工具按 ID 查询。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiSelectionKind {
    Transaction,
    StreamPacket,
    Account,
    RuleSet,
}

/// 保存当前页面选中的稳定资源标识；不会复制事务正文、账号资料或规则正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiDataSelection {
    pub kind: UiSelectionKind,
    pub ids: Vec<String>,
    pub side: Option<String>,
    pub sequence: Option<u64>,
}

/// 接收单个窗口的单调更新；sequence 用于丢弃慢请求覆盖新界面的网络竞态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiContextUpdate {
    pub instanceId: Uuid,
    pub sequence: u64,
    pub windowKind: UiWindowKind,
    pub page: UiPage,
    pub section: Option<String>,
    pub view: Option<String>,
    pub selection: Option<UiDataSelection>,
    pub focused: bool,
    pub visible: bool,
}

/// 返回服务端确认的窗口上下文；更新时间由控制服务生成，不能由浏览器伪造排序。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContext {
    pub instanceId: Uuid,
    pub sequence: u64,
    pub windowKind: UiWindowKind,
    pub page: UiPage,
    pub section: Option<String>,
    pub view: Option<String>,
    pub selection: Option<UiDataSelection>,
    pub focused: bool,
    pub visible: bool,
    pub updatedAtMilliseconds: u64,
}

/// 聚合所有仍活跃的窗口，并给 MCP 提供一个按焦点、可见性和新鲜度选择的主上下文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContextSnapshot {
    pub primary: Option<UiContext>,
    pub contexts: Vec<UiContext>,
}

/// 持有进程内界面心跳；服务重启或窗口停止心跳后数据自然消失，不污染持久配置。
#[derive(Clone, Default)]
pub(super) struct UiContextRegistry {
    contexts: Arc<RwLock<HashMap<Uuid, UiContext>>>,
}

impl UiContextRegistry {
    /// 校验并写入一个窗口更新；旧 sequence 会被忽略，容量满时淘汰最久未更新的窗口。
    ///
    /// 参数 `update` 只允许有界稳定标识；字段非法时返回配置请求错误，不改变已有上下文。
    async fn update(&self, update: UiContextUpdate) -> Result<UiContextSnapshot, ApiError> {
        validateUpdate(&update)?;
        let now = currentTimeMilliseconds();
        let mut contexts = self.contexts.write().await;
        pruneExpired(&mut contexts, now);
        if contexts
            .get(&update.instanceId)
            .is_some_and(|current| current.sequence >= update.sequence)
        {
            return Ok(buildSnapshot(&contexts));
        }
        if contexts.len() >= maximumUiContexts
            && !contexts.contains_key(&update.instanceId)
            && let Some(oldest) = contexts
                .values()
                .min_by_key(|context| context.updatedAtMilliseconds)
                .map(|context| context.instanceId)
        {
            contexts.remove(&oldest);
        }
        contexts.insert(update.instanceId, UiContext::fromUpdate(update, now));
        Ok(buildSnapshot(&contexts))
    }

    /// 返回活跃窗口快照并清除超时心跳；读取不会推进业务 revision 或发布事件。
    async fn snapshot(&self) -> UiContextSnapshot {
        let now = currentTimeMilliseconds();
        let mut contexts = self.contexts.write().await;
        pruneExpired(&mut contexts, now);
        buildSnapshot(&contexts)
    }
}

impl UiContext {
    /// 把已验证的浏览器更新转换为服务端时间戳上下文。
    ///
    /// 运行上下文：仅由注册表持有写锁时调用；`now` 是同一提交使用的控制服务毫秒时钟。
    /// 该纯转换不会失败，也不会保留请求之外的临时数据。
    fn fromUpdate(update: UiContextUpdate, now: u64) -> Self {
        Self {
            instanceId: update.instanceId,
            sequence: update.sequence,
            windowKind: update.windowKind,
            page: update.page,
            section: update.section,
            view: update.view,
            selection: update.selection,
            focused: update.focused,
            visible: update.visible,
            updatedAtMilliseconds: now,
        }
    }
}

/// 校验上报只包含有界标识和合法消息侧。
///
/// 运行上下文：注册表取得写锁前调用；`update` 是尚未进入共享状态的请求对象。
/// 任一边界不满足时返回不回显字段值的 400 错误，已有窗口状态保持不变。
fn validateUpdate(update: &UiContextUpdate) -> Result<(), ApiError> {
    if update.sequence == 0
        || !validQualifier(update.section.as_deref())
        || !validQualifier(update.view.as_deref())
    {
        return Err(invalidUiContext());
    }
    let Some(selection) = &update.selection else {
        return Ok(());
    };
    if selection.ids.is_empty()
        || selection.ids.len() > maximumSelectionIds
        || selection.ids.iter().any(|value| !validIdentifier(value))
        || !matches!(
            selection.side.as_deref(),
            None | Some("request") | Some("response")
        )
        || (selection.kind == UiSelectionKind::StreamPacket
            && (selection.ids.len() != 1
                || selection.side.is_none()
                || selection.sequence.is_none()))
    {
        return Err(invalidUiContext());
    }
    Ok(())
}

/// 判断可选页面限定符是否为可安全展示的短 token。
///
/// 参数 `value` 来自路由 section 或视图名；空值必须由 None 表示。函数无失败副作用，
/// 只在字段为空、超长或含非 ASCII token 字符时返回 false。
fn validQualifier(value: Option<&str>) -> bool {
    value.is_none_or(|candidate| {
        !candidate.is_empty()
            && candidate.len() <= maximumQualifierBytes
            && candidate
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
    })
}

/// 判断资源标识是否非空、有界且不含控制字符。
///
/// 参数 `value` 是事务、账号或规则集的稳定 ID；函数不规范化原值，非法时仅返回 false。
fn validIdentifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= maximumIdentifierBytes
        && !value.chars().any(char::is_control)
}

/// 构造统一请求错误；调用点不传参数，因此具体无效值不会进入响应或日志。
fn invalidUiContext() -> ApiError {
    ApiError::badRequest(ErrorCode::InvalidConfigurationRequest)
}

/// 删除超过心跳窗口的上下文。
///
/// 运行上下文：调用方必须持注册表写锁；`now` 使用控制服务时钟。该操作不失败，客户端
/// 离线、崩溃或页面关闭均通过同一规则收敛。
fn pruneExpired(contexts: &mut HashMap<Uuid, UiContext>, now: u64) {
    contexts.retain(|_, context| {
        now.saturating_sub(context.updatedAtMilliseconds) <= uiContextLifetimeMilliseconds
    });
}

/// 按焦点、可见性和服务端时间选择主窗口，并稳定排序其余窗口。
///
/// 参数 `contexts` 是已完成过期清理的锁内快照；克隆分配失败由 Rust 进程级内存边界处理，
/// 正常路径不返回错误，排序稳定便于 MCP 重复读取比较。
fn buildSnapshot(contexts: &HashMap<Uuid, UiContext>) -> UiContextSnapshot {
    let mut ordered: Vec<_> = contexts.values().cloned().collect();
    ordered.sort_by(|left, right| {
        (
            right.focused,
            right.visible,
            right.updatedAtMilliseconds,
            right.sequence,
        )
            .cmp(&(
                left.focused,
                left.visible,
                left.updatedAtMilliseconds,
                left.sequence,
            ))
            .then_with(|| left.instanceId.cmp(&right.instanceId))
    });
    UiContextSnapshot {
        primary: ordered.first().cloned(),
        contexts: ordered,
    }
}

/// 装配界面上下文读写路由；参数 `router` 是尚未绑定 ControlState 的控制路由。
/// PUT 更新心跳，GET 只返回当前活跃窗口；装配本身不执行 I/O，也没有运行时失败分支。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router.route("/api/v1/ui/context", get(getUiContext).put(updateUiContext))
}

/// 返回当前活跃界面；参数 `state` 提供进程内注册表，该处理器只会清理过期项，
/// 不触发快照锁或业务事件，成功始终返回结构化集合。
async fn getUiContext(State(state): State<ControlState>) -> Json<UiContextSnapshot> {
    Json(state.uiContexts.snapshot().await)
}

/// 接收浏览器心跳并返回服务端确认后的聚合视图；JSON 或字段校验失败返回稳定 400。
async fn updateUiContext(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<UiContextUpdate>, JsonRejection>,
) -> Result<Json<UiContextSnapshot>, LocalizedApiError> {
    let Json(update) = updateResult.map_err(|_| invalidUiContext().withLocale(locale))?;
    state
        .uiContexts
        .update(update)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}
