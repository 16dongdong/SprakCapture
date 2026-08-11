use capture_core::TransactionSummary;
use serde::Serialize;

const maximumTransactionCollectionBytes: usize = 4 * 1024 * 1024 - 64 * 1024;

/// 返回有界事务摘要集合；items 永远不包含头字段或正文。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionPage {
    pub revision: u64,
    pub recordingSessionId: String,
    pub collectionToken: String,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub hasPrevious: bool,
    pub hasMore: bool,
    pub nextOffset: Option<usize>,
    pub truncated: bool,
    pub itemsTruncated: bool,
    pub items: Vec<TransactionSummary>,
}

/// 聚合事务页构造所需的线性化 Capture 视图和控制面修订号，避免位置参数错配。
pub struct TransactionPageSource {
    pub revision: u64,
    pub recordingSessionId: String,
    pub collectionToken: String,
    pub total: usize,
    pub transactions: Vec<TransactionSummary>,
    pub offset: usize,
    pub limit: usize,
    pub preferLatest: bool,
}

/// 构造有 4MiB 序列化预算的事务集合；最新页从尾部装入，保证预算不足时优先保留最新记录。
pub fn buildTransactionPage(source: TransactionPageSource) -> TransactionPage {
    let TransactionPageSource {
        revision,
        recordingSessionId,
        collectionToken,
        total,
        transactions,
        offset,
        limit,
        preferLatest,
    } = source;
    let candidateCount = transactions.len();
    let mut items = Vec::new();
    let mut serializedBytes = 0_usize;
    let mut itemsTruncated = false;
    if preferLatest {
        for transaction in transactions.into_iter().rev() {
            if !appendBoundedTransaction(
                &mut items,
                &mut serializedBytes,
                &mut itemsTruncated,
                transaction,
            ) {
                break;
            }
        }
        items.reverse();
    } else {
        for transaction in transactions {
            if !appendBoundedTransaction(
                &mut items,
                &mut serializedBytes,
                &mut itemsTruncated,
                transaction,
            ) {
                break;
            }
        }
    }
    let actualOffset = if preferLatest {
        offset.saturating_add(candidateCount.saturating_sub(items.len()))
    } else {
        offset
    };
    let returnedEnd = actualOffset.saturating_add(items.len());
    let hasMore = returnedEnd < total;
    TransactionPage {
        revision,
        recordingSessionId,
        collectionToken,
        total,
        offset: actualOffset,
        limit,
        hasPrevious: actualOffset > 0,
        hasMore,
        // 4 MiB 投影预算可能使实际条数小于请求 limit，必须返回真实末端而不是让调用方猜测步长。
        nextOffset: hasMore.then_some(returnedEnd),
        truncated: actualOffset > 0 || hasMore || itemsTruncated,
        itemsTruncated,
        items,
    }
}

/// 将一个摘要裁剪后追加到固定预算；预算不足时返回 false，调用方据此停止遍历剩余页面。
fn appendBoundedTransaction(
    items: &mut Vec<TransactionSummary>,
    serializedBytes: &mut usize,
    itemsTruncated: &mut bool,
    transaction: TransactionSummary,
) -> bool {
    let (mut bounded, fieldsTruncated) = boundTransactionSummary(transaction);
    let mut transactionBytes = serializedLength(&bounded);
    if transactionBytes > maximumTransactionCollectionBytes {
        bounded = minimalTransactionSummary(bounded);
        transactionBytes = serializedLength(&bounded);
        *itemsTruncated = true;
    }
    if serializedBytes.saturating_add(transactionBytes) > maximumTransactionCollectionBytes {
        return false;
    }
    *serializedBytes += transactionBytes;
    *itemsTruncated |= fieldsTruncated;
    items.push(bounded);
    true
}

/// 序列化单个已裁剪摘要；输入上界由 boundTransactionSummary 保证，因此临时分配始终有界。
fn serializedLength(transaction: &TransactionSummary) -> usize {
    serde_json::to_vec(transaction)
        .expect("TransactionSummary 必须可序列化")
        .len()
}

/// 将列表级自由文本裁剪到确定边界并移除控制字符，保证二次序列化始终有界。
fn boundTransactionSummary(mut transaction: TransactionSummary) -> (TransactionSummary, bool) {
    let mut truncated = false;
    truncated |= boundString(&mut transaction.method, 64);
    truncated |= boundString(&mut transaction.host, 256);
    truncated |= boundString(&mut transaction.path, 1_024);
    truncated |= boundString(&mut transaction.query, 1_024);
    truncated |= boundString(&mut transaction.urlDisplay, 2_048);
    truncated |= boundString(&mut transaction.clientAddress, 128);
    if let Some(processName) = transaction.clientProcessName.as_mut() {
        truncated |= boundString(processName, 256);
    }
    truncated |= boundString(&mut transaction.contentType, 128);
    truncated |= boundString(&mut transaction.notes, 512);
    truncated |= boundStrings(&mut transaction.tags, 8, 64);
    truncated |= boundStrings(&mut transaction.appliedTools, 8, 64);
    if let Some(error) = transaction.error.as_mut() {
        truncated |= boundString(&mut error.code, 64);
        truncated |= boundString(&mut error.messageKey, 128);
        let originalCount = error.params.len();
        error.params = error
            .params
            .iter()
            .take(4)
            .map(|(name, value)| {
                let mut name = name.clone();
                let mut value = value.clone();
                truncated |= boundString(&mut name, 32);
                truncated |= boundString(&mut value, 128);
                (name, value)
            })
            .collect();
        truncated |= originalCount != error.params.len();
    }
    (transaction, truncated)
}

/// 将单个 UTF-8 字段裁剪到字符边界并替换控制字符；返回值表示公开投影发生变化。
fn boundString(value: &mut String, maximumBytes: usize) -> bool {
    let originalLength = value.len();
    let mut changed = false;
    let mut bounded = String::with_capacity(value.len().min(maximumBytes));
    for character in value.chars() {
        let character = if character.is_control() {
            changed = true;
            '?'
        } else {
            character
        };
        if bounded.len().saturating_add(character.len_utf8()) > maximumBytes {
            changed = true;
            break;
        }
        bounded.push(character);
    }
    *value = bounded;
    changed || value.len() != originalLength
}

/// 限制字符串数组的数量和单项长度；列表层只保留足以预览的标签和工具名。
fn boundStrings(values: &mut Vec<String>, maximumItems: usize, maximumBytes: usize) -> bool {
    let mut truncated = values.len() > maximumItems;
    values.truncate(maximumItems);
    for value in values {
        truncated |= boundString(value, maximumBytes);
    }
    truncated
}

/// 将极端单项退化为只含协议事实的有界投影；UUID、数值、枚举和状态仍完整保留。
fn minimalTransactionSummary(mut transaction: TransactionSummary) -> TransactionSummary {
    transaction.method.clear();
    transaction.host.clear();
    transaction.path.clear();
    transaction.query.clear();
    transaction.urlDisplay.clear();
    transaction.clientAddress.clear();
    transaction.clientProcessName = None;
    transaction.contentType.clear();
    transaction.error = None;
    transaction.notes.clear();
    transaction.tags.clear();
    transaction.appliedTools.clear();
    transaction
}
