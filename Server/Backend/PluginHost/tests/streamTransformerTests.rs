#![allow(non_snake_case)]

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use plugin_host::{
    DecodedFrame, StreamDirection, StreamInput, StreamOutputFrame, StreamTransformDecision,
    StreamTransformError, StreamTransformer, StreamTransformerSession,
};

/// 模拟长度前缀协议的增量转换器；每个完整帧转为大写正文并重新生成长度字段。
struct LengthPrefixedTransformer {
    closeCount: AtomicUsize,
    observedWindows: Mutex<VecDeque<Vec<u8>>>,
}

impl LengthPrefixedTransformer {
    /// 创建没有连接状态的测试转换器；窗口记录用于证明半包不会提前发布。
    fn new() -> Self {
        Self {
            closeCount: AtomicUsize::new(0),
            observedWindows: Mutex::new(VecDeque::new()),
        }
    }
}

impl StreamTransformer for LengthPrefixedTransformer {
    /// 解析一字节长度头并产生完整重封包；不足时精确请求剩余字节。
    fn transform<'a>(
        &'a self,
        input: StreamInput<'a>,
    ) -> Pin<
        Box<dyn Future<Output = Result<StreamTransformDecision, StreamTransformError>> + Send + 'a>,
    > {
        self.observedWindows.lock().push_back(input.bytes.to_vec());
        Box::pin(async move {
            let Some(payloadLength) = input.bytes.first().copied().map(usize::from) else {
                return Ok(StreamTransformDecision::NeedMore {
                    minimumAdditionalBytes: 1,
                });
            };
            let frameLength = payloadLength + 1;
            if input.bytes.len() < frameLength {
                return Ok(StreamTransformDecision::NeedMore {
                    minimumAdditionalBytes: frameLength - input.bytes.len(),
                });
            }
            let mut wireBytes = Vec::with_capacity(frameLength);
            wireBytes.push(payloadLength as u8);
            wireBytes.extend(
                input.bytes[1..frameLength]
                    .iter()
                    .map(u8::to_ascii_uppercase),
            );
            Ok(StreamTransformDecision::Emit {
                consumedBytes: frameLength,
                outputFrames: vec![StreamOutputFrame {
                    wireBytes,
                    decoded: Some(DecodedFrame {
                        frameType: "message".to_owned(),
                        protocolVersion: Some("1".to_owned()),
                        correlationId: None,
                        fields: Vec::new(),
                        annotations: Vec::new(),
                    }),
                }],
            })
        })
    }

    /// 记录连接状态清理次数；宿主必须保证显式关闭和析构合计只通知一次。
    fn close(&self, _connectionId: u64) {
        self.closeCount.fetch_add(1, Ordering::AcqRel);
    }
}

#[tokio::test]
async fn holdsPartialFrameAndPublishesOnlyCompleteRepackedBytes() {
    let transformer = Arc::new(LengthPrefixedTransformer::new());
    let session =
        StreamTransformerSession::new(7, transformer, Duration::from_millis(100), 1024, 1024)
            .expect("创建转换会话");

    assert!(
        session
            .push(StreamDirection::ClientToServer, &[5, b'h', b'e'])
            .await
            .expect("写入半包")
            .is_empty()
    );
    let output = session
        .push(StreamDirection::ClientToServer, b"llo")
        .await
        .expect("完成分包和重封包");
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].wireBytes, [5, b'H', b'E', b'L', b'L', b'O']);
}

#[tokio::test]
async fn extractsMultipleStickyFramesInStableOrder() {
    let transformer = Arc::new(LengthPrefixedTransformer::new());
    let session =
        StreamTransformerSession::new(8, transformer, Duration::from_millis(100), 1024, 1024)
            .expect("创建转换会话");

    let output = session
        .push(
            StreamDirection::ServerToClient,
            &[3, b'o', b'n', b'e', 3, b't', b'w', b'o'],
        )
        .await
        .expect("解析粘包");
    assert_eq!(output.len(), 2);
    assert_eq!(output[0].wireBytes, [3, b'O', b'N', b'E']);
    assert_eq!(output[1].wireBytes, [3, b'T', b'W', b'O']);
}

#[tokio::test]
async fn rejectsIncompleteFrameAtHalfCloseWithoutPublishingPartialOutput() {
    let transformer = Arc::new(LengthPrefixedTransformer::new());
    let session =
        StreamTransformerSession::new(9, transformer, Duration::from_millis(100), 1024, 1024)
            .expect("创建转换会话");

    session
        .push(StreamDirection::ClientToServer, &[4, b'a'])
        .await
        .expect("写入半包");
    assert_eq!(
        session.finish(StreamDirection::ClientToServer).await,
        Err(StreamTransformError::InvalidDecision)
    );
}

#[test]
fn closesTransformerExactlyOnce() {
    let transformer = Arc::new(LengthPrefixedTransformer::new());
    {
        let session = StreamTransformerSession::new(
            10,
            transformer.clone(),
            Duration::from_millis(100),
            1024,
            1024,
        )
        .expect("创建转换会话");
        session.close();
        session.close();
    }
    assert_eq!(transformer.closeCount.load(Ordering::Acquire), 1);
}
