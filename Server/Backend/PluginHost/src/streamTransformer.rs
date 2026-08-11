//! 提供私有 TCP 二进制协议所需的双向、有状态、原子帧转换器。
//!
//! TCP 读取块不等于应用层帧。本模块先在连接级有界缓冲中重组，再让插件完成解密、结构化
//! 修改、重新编码和认证；只有整个决定通过宿主校验后才向调用方发布输出字节。

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio::{
    sync::Mutex,
    time::{Duration, timeout},
};

use crate::StreamDirection;

const MAXIMUM_STREAM_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_OUTPUT_FRAMES_PER_DECISION: usize = 1_024;
const MAXIMUM_DECODED_FIELDS_PER_FRAME: usize = 16_384;

/// 保存解密后协议树中的一个字段；偏移和长度指向当前完整明文帧。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodedField {
    pub path: String,
    pub valueType: String,
    pub value: JsonValue,
    pub offset: usize,
    pub length: usize,
    #[serde(default)]
    pub editable: bool,
}

/// 保存插件识别出的完整应用层帧；展示字段与最终线上字节分离，避免 UI 反向修改缓冲区。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodedFrame {
    pub frameType: String,
    #[serde(default)]
    pub protocolVersion: Option<String>,
    #[serde(default)]
    pub correlationId: Option<String>,
    #[serde(default)]
    pub fields: Vec<DecodedField>,
    #[serde(default)]
    pub annotations: Vec<JsonValue>,
}

/// 保存一个已经完成重编码、加密、认证和校验的输出帧；宿主不会再次猜测其内部格式。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamOutputFrame {
    pub wireBytes: Vec<u8>,
    #[serde(default)]
    pub decoded: Option<DecodedFrame>,
}

/// 描述传给插件的当前方向连续字节窗口；同一连接的两个方向共享插件连接状态但缓冲互相独立。
#[derive(Clone, Debug)]
pub struct StreamInput<'a> {
    pub connectionId: u64,
    pub direction: StreamDirection,
    pub bytes: &'a [u8],
    pub endOfStream: bool,
}

/// 描述插件完成一次增量解析后的决定。
#[derive(Clone, Debug, PartialEq)]
pub enum StreamTransformDecision {
    NeedMore {
        minimumAdditionalBytes: usize,
    },
    Emit {
        consumedBytes: usize,
        outputFrames: Vec<StreamOutputFrame>,
    },
    Drop {
        consumedBytes: usize,
        decoded: Option<DecodedFrame>,
    },
    Close {
        reason: String,
    },
}

/// 描述流转换器或宿主边界校验失败；任何失败都发生在输出字节发布之前。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum StreamTransformError {
    #[error("streamBufferExceeded")]
    BufferExceeded,
    #[error("streamTransformerTimeout")]
    Timeout,
    #[error("streamTransformerFailed")]
    RuntimeFailed,
    #[error("streamTransformerInvalidDecision")]
    InvalidDecision,
    #[error("streamTransformerClosed")]
    Closed,
}

/// 定义一个连接级双向流转换器；实现负责握手、密钥、计数器、压缩和请求响应关联状态。
pub trait StreamTransformer: Send + Sync {
    /// 对当前方向的连续字节窗口执行增量解析和重封包；返回前不得向网络写入任何数据。
    fn transform<'a>(
        &'a self,
        input: StreamInput<'a>,
    ) -> Pin<
        Box<dyn Future<Output = Result<StreamTransformDecision, StreamTransformError>> + Send + 'a>,
    >;

    /// 通知连接关闭并立即清理会话密钥与私有状态；实现不得等待网络 I/O。
    fn close(&self, connectionId: u64);
}

/// 保存连接单方向的连续输入缓冲；同方向调用使用异步互斥保证解析顺序稳定。
struct DirectionBuffer {
    bytes: Vec<u8>,
    endOfStream: bool,
}

impl DirectionBuffer {
    /// 创建空方向缓冲；容量只随真实半包增长，不为透明连接预分配大块内存。
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            endOfStream: false,
        }
    }
}

/// 管理一条连接的双向转换生命周期；调用方按原方向顺序写入返回的完整帧列表。
pub struct StreamTransformerSession {
    connectionId: u64,
    transformer: Arc<dyn StreamTransformer>,
    directions: [Mutex<DirectionBuffer>; 2],
    timeout: Duration,
    maximumBufferedBytes: usize,
    maximumOutputBytes: usize,
    closed: AtomicBool,
}

impl StreamTransformerSession {
    /// 创建连接级转换会话；预算必须非零且不得超过宿主硬上限。
    pub fn new(
        connectionId: u64,
        transformer: Arc<dyn StreamTransformer>,
        timeout: Duration,
        maximumBufferedBytes: usize,
        maximumOutputBytes: usize,
    ) -> Result<Self, StreamTransformError> {
        if timeout.is_zero()
            || maximumBufferedBytes == 0
            || maximumBufferedBytes > MAXIMUM_STREAM_BUFFER_BYTES
            || maximumOutputBytes == 0
        {
            return Err(StreamTransformError::InvalidDecision);
        }
        Ok(Self {
            connectionId,
            transformer,
            directions: [
                Mutex::new(DirectionBuffer::new()),
                Mutex::new(DirectionBuffer::new()),
            ],
            timeout,
            maximumBufferedBytes,
            maximumOutputBytes,
            closed: AtomicBool::new(false),
        })
    }

    /// 追加一个 TCP 读取块并持续提取所有完整帧；插件失败时不返回任何部分输出。
    pub async fn push(
        &self,
        direction: StreamDirection,
        bytes: &[u8],
    ) -> Result<Vec<StreamOutputFrame>, StreamTransformError> {
        self.process(direction, bytes, false).await
    }

    /// 发布指定方向的半关闭；剩余字节必须被完整消费，否则返回结构错误而不是静默丢弃。
    pub async fn finish(
        &self,
        direction: StreamDirection,
    ) -> Result<Vec<StreamOutputFrame>, StreamTransformError> {
        self.process(direction, &[], true).await
    }

    /// 关闭整个转换会话；该操作幂等，首次调用负责通知插件清除连接状态。
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.transformer.close(self.connectionId);
        }
    }

    /// 在单方向锁内执行追加、解析和原子输出校验；对端方向仍可并发推进共享协议状态。
    async fn process(
        &self,
        direction: StreamDirection,
        bytes: &[u8],
        endOfStream: bool,
    ) -> Result<Vec<StreamOutputFrame>, StreamTransformError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(StreamTransformError::Closed);
        }
        let slot = match direction {
            StreamDirection::ClientToServer => 0,
            StreamDirection::ServerToClient => 1,
        };
        let mut directionBuffer = self.directions[slot].lock().await;
        let nextLength = directionBuffer
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(StreamTransformError::BufferExceeded)?;
        if nextLength > self.maximumBufferedBytes {
            return Err(StreamTransformError::BufferExceeded);
        }
        directionBuffer.bytes.extend_from_slice(bytes);
        directionBuffer.endOfStream |= endOfStream;

        let mut stagedOutput = Vec::new();
        let mut stagedOutputBytes = 0_usize;
        if directionBuffer.bytes.is_empty() {
            return Ok(stagedOutput);
        }
        loop {
            let input = StreamInput {
                connectionId: self.connectionId,
                direction,
                bytes: directionBuffer.bytes.as_slice(),
                endOfStream: directionBuffer.endOfStream,
            };
            let decision = timeout(self.timeout, self.transformer.transform(input))
                .await
                .map_err(|_| StreamTransformError::Timeout)??;
            match decision {
                StreamTransformDecision::NeedMore {
                    minimumAdditionalBytes,
                } => {
                    if minimumAdditionalBytes == 0
                        || directionBuffer.endOfStream
                        || directionBuffer
                            .bytes
                            .len()
                            .checked_add(minimumAdditionalBytes)
                            .is_none_or(|length| length > self.maximumBufferedBytes)
                    {
                        return Err(StreamTransformError::InvalidDecision);
                    }
                    break;
                }
                StreamTransformDecision::Emit {
                    consumedBytes,
                    outputFrames,
                } => {
                    validateDecision(
                        directionBuffer.bytes.len(),
                        consumedBytes,
                        &outputFrames,
                        self.maximumOutputBytes.saturating_sub(stagedOutputBytes),
                    )?;
                    stagedOutputBytes = stagedOutputBytes.saturating_add(
                        outputFrames
                            .iter()
                            .map(|frame| frame.wireBytes.len())
                            .sum::<usize>(),
                    );
                    directionBuffer.bytes.drain(..consumedBytes);
                    stagedOutput.extend(outputFrames);
                }
                StreamTransformDecision::Drop {
                    consumedBytes,
                    decoded,
                } => {
                    validateConsumed(directionBuffer.bytes.len(), consumedBytes)?;
                    validateDecoded(decoded.as_ref())?;
                    directionBuffer.bytes.drain(..consumedBytes);
                }
                StreamTransformDecision::Close { .. } => {
                    return Err(StreamTransformError::Closed);
                }
            }
            if directionBuffer.bytes.is_empty() {
                break;
            }
        }
        if directionBuffer.endOfStream && !directionBuffer.bytes.is_empty() {
            return Err(StreamTransformError::InvalidDecision);
        }
        Ok(stagedOutput)
    }
}

impl Drop for StreamTransformerSession {
    /// 在调用方遗漏显式关闭时仍清除插件连接状态；析构路径不执行异步工作。
    fn drop(&mut self) {
        self.close();
    }
}

/// 验证 Emit 决定的消费范围、帧数、总输出和解码字段边界。
fn validateDecision(
    availableBytes: usize,
    consumedBytes: usize,
    outputFrames: &[StreamOutputFrame],
    remainingOutputBytes: usize,
) -> Result<(), StreamTransformError> {
    validateConsumed(availableBytes, consumedBytes)?;
    if outputFrames.is_empty() || outputFrames.len() > MAXIMUM_OUTPUT_FRAMES_PER_DECISION {
        return Err(StreamTransformError::InvalidDecision);
    }
    let outputBytes = outputFrames.iter().try_fold(0_usize, |total, frame| {
        validateDecoded(frame.decoded.as_ref())?;
        total
            .checked_add(frame.wireBytes.len())
            .ok_or(StreamTransformError::InvalidDecision)
    })?;
    if outputBytes > remainingOutputBytes {
        return Err(StreamTransformError::InvalidDecision);
    }
    Ok(())
}

/// 验证插件至少消费一个且不超过当前窗口的字节，防止零进度死循环和越界删除。
fn validateConsumed(
    availableBytes: usize,
    consumedBytes: usize,
) -> Result<(), StreamTransformError> {
    if consumedBytes == 0 || consumedBytes > availableBytes {
        return Err(StreamTransformError::InvalidDecision);
    }
    Ok(())
}

/// 验证解码字段数量和明文偏移算术；正文长度由协议插件自己的 Schema 负责解释。
fn validateDecoded(decoded: Option<&DecodedFrame>) -> Result<(), StreamTransformError> {
    let Some(decoded) = decoded else {
        return Ok(());
    };
    if decoded.frameType.is_empty() || decoded.fields.len() > MAXIMUM_DECODED_FIELDS_PER_FRAME {
        return Err(StreamTransformError::InvalidDecision);
    }
    if decoded
        .fields
        .iter()
        .any(|field| field.path.is_empty() || field.offset.checked_add(field.length).is_none())
    {
        return Err(StreamTransformError::InvalidDecision);
    }
    Ok(())
}
