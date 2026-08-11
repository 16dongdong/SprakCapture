use std::mem::size_of;

use crate::{
    BeginTransaction, BodyWrite, HeaderField, TransactionCompletion, TransactionError,
    TransactionSummary, TransactionUpdate, TransactionUserUpdate,
};

/// 单条事务摘要使用 JavaScript 安全整数边界；真实系统资源不足必须作为显式错误返回。
pub(crate) const maximumTransactionSummaryBytes: usize = 9_007_199_254_740_991;
/// 显式测试配置至少预留 64KiB，避免创建一个连最小事务都无法登记的伪成功会话。
pub(crate) const minimumMetadataMemoryBudgetBytes: usize = 64 * 1024;
/// 元数据采用物理不可达记账边界，禁止因内存预算静默淘汰已经录制的事务。
pub(crate) const defaultMetadataMemoryBudgetBytes: usize = 9_007_199_254_740_991;
/// 单侧头保存完整字段；数据面已在协议解析阶段验证合法长度，录制层不得再次裁剪。
const maximumStoredHeaderSideBytes: usize = 9_007_199_254_740_991;
/// 头名称只受协议解析和 JavaScript 精确计数边界约束，不在录制层丢失字节。
const maximumHeaderNameBytes: usize = 9_007_199_254_740_991;
/// 头值完整持久化；磁盘或内存错误显式终止当前录制写入，不能生成截断事务。
const maximumHeaderValueBytes: usize = 9_007_199_254_740_991;

/// 保存头裁剪结果及其精确逻辑内存占用；truncated 用于驱动公开事务标志。
pub(crate) struct BoundedHeaders {
    pub headers: Vec<HeaderField>,
    pub storedBytes: usize,
    pub truncated: bool,
}

/// 保留事务创建输入的完整字段；协议层负责合法性，录制层不得二次裁剪可见事务。
pub(crate) fn boundBeginTransaction(input: BeginTransaction) -> BeginTransaction {
    input
}

/// 保留协议增量的完整字段；资源不足由写入操作显式返回，不能修改线上观察结果。
pub(crate) fn boundTransactionUpdate(update: TransactionUpdate) -> TransactionUpdate {
    update
}

/// 保留用户备注、标签与工具名的完整内容，使会话视图和导出结果保持一致。
pub(crate) fn boundUserUpdate(update: TransactionUserUpdate) -> TransactionUserUpdate {
    update
}

/// 保留终态字段的完整内容；状态提交不得产生与真实响应不同的摘要。
pub(crate) fn boundCompletion(completion: TransactionCompletion) -> TransactionCompletion {
    completion
}

/// 保留事务错误和参数的完整内容，确保诊断信息不会因录制预算而丢失。
pub(crate) fn boundTransactionError(error: TransactionError) -> TransactionError {
    error
}

/// 保留正文元信息和完整字节；正文持久化只能成功或返回明确 I/O 错误。
pub(crate) fn boundBodyWrite(body: BodyWrite) -> BodyWrite {
    body
}

/// 按单侧固定上限和当前全局可用预算裁剪头，保留原始字段顺序及允许范围内的重复项。
pub(crate) fn boundHeaders(headers: Vec<HeaderField>, availableBytes: usize) -> BoundedHeaders {
    let maximumBytes = availableBytes.min(maximumStoredHeaderSideBytes);
    let originalCount = headers.len();
    let mut boundedHeaders = Vec::new();
    let mut logicalBytes = 0_usize;
    let mut truncated = false;
    for header in headers {
        let (name, nameTruncated) = boundString(header.name, maximumHeaderNameBytes);
        let (mut value, valueTruncated) = boundString(header.value, maximumHeaderValueBytes);
        truncated |= nameTruncated || valueTruncated;
        let fixedBytes = size_of::<HeaderField>().saturating_add(name.len());
        if logicalBytes.saturating_add(fixedBytes) > maximumBytes {
            truncated = true;
            break;
        }
        let remainingValueBytes = maximumBytes
            .saturating_sub(logicalBytes)
            .saturating_sub(fixedBytes);
        if value.len() > remainingValueBytes {
            value = boundString(value, remainingValueBytes).0;
            truncated = true;
        }
        logicalBytes = logicalBytes
            .saturating_add(fixedBytes)
            .saturating_add(value.len());
        boundedHeaders.push(HeaderField { name, value });
        if logicalBytes == maximumBytes {
            truncated |= boundedHeaders.len() < originalCount;
            break;
        }
    }
    truncated |= boundedHeaders.len() < originalCount;
    boundedHeaders.shrink_to_fit();
    let mut storedBytes = headerStorageBytes(&boundedHeaders, boundedHeaders.capacity());
    while storedBytes > maximumBytes {
        boundedHeaders.pop();
        boundedHeaders.shrink_to_fit();
        storedBytes = headerStorageBytes(&boundedHeaders, boundedHeaders.capacity());
        truncated = true;
    }
    BoundedHeaders {
        headers: boundedHeaders,
        storedBytes,
        truncated,
    }
}

/// 返回持久头的逻辑内存占用；所有存入集合的头都由 boundHeaders 构造。
pub(crate) fn headerStorageBytes(headers: &[HeaderField], allocatedSlots: usize) -> usize {
    headers.iter().fold(
        allocatedSlots.saturating_mul(size_of::<HeaderField>()),
        |total, header| {
            total
                .saturating_add(header.name.capacity())
                .saturating_add(header.value.capacity())
        },
    )
}

/// 精确计算摘要结构、字符串和集合项的持久逻辑字节，用于每次摘要变更后的全局记账。
pub(crate) fn summaryStorageBytes(summary: &TransactionSummary) -> usize {
    let mut total = size_of::<TransactionSummary>();
    for value in [
        &summary.transactionId,
        &summary.recordingSessionId,
        &summary.method,
        &summary.host,
        &summary.path,
        &summary.query,
        &summary.urlDisplay,
        &summary.clientAddress,
        &summary.contentType,
        &summary.notes,
    ] {
        total = total.saturating_add(value.capacity());
    }
    if let Some(processName) = summary.clientProcessName.as_ref() {
        total = total.saturating_add(processName.capacity());
    }
    total = total
        .saturating_add(stringListStorageBytes(
            &summary.tags,
            summary.tags.capacity(),
        ))
        .saturating_add(stringListStorageBytes(
            &summary.appliedTools,
            summary.appliedTools.capacity(),
        ));
    if let Some(error) = summary.error.as_ref() {
        total = total
            .saturating_add(size_of::<TransactionError>())
            .saturating_add(error.code.capacity())
            .saturating_add(error.messageKey.capacity());
        for (name, value) in &error.params {
            total = total
                .saturating_add(size_of::<(String, String)>())
                .saturating_add(name.capacity())
                .saturating_add(value.capacity());
        }
    }
    total
}

/// 在 JavaScript 安全整数边界内原样保留 UTF-8；该边界只防止跨语言计数失真，不参与正常录制裁剪。
fn boundString(value: String, maximumBytes: usize) -> (String, bool) {
    if value.len() <= maximumBytes {
        return (value, false);
    }
    let originalLength = value.len();
    let mut bounded = String::with_capacity(maximumBytes);
    for character in value.chars() {
        if bounded.len().saturating_add(character.len_utf8()) > maximumBytes {
            break;
        }
        bounded.push(character);
    }
    bounded.shrink_to_fit();
    (bounded, originalLength > maximumBytes)
}

/// 计算字符串集合的固定项结构和实际文本长度；Vec 自身已包含在 TransactionSummary 中。
fn stringListStorageBytes(values: &[String], allocatedSlots: usize) -> usize {
    values.iter().fold(
        allocatedSlots.saturating_mul(size_of::<String>()),
        |total, value| total.saturating_add(value.capacity()),
    )
}
