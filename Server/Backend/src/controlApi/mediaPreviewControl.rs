//! 为媒体查看器提供只读 HTTP Range 重组，不改变录制事务和正文。
//!
//! CDN 常把一个音视频资源拆成多个 206 事务；单个非零分段缺少容器初始化信息，直接
//! 交给浏览器必然产生解码错误。本模块只在完整 URL、强 ETag、总长度和连续范围均一致时
//! 建立虚拟正文清单，并在 Axum 轮询响应时从稳定只读租约逐块读取。租约在计划阶段一次性
//! 固定内存 Arc 或打开 spill 句柄，FIFO 淘汰与 clear 不会截断活动响应。控制响应不生成
//! Base64、不聚合完整媒体，也不修改原始事务、正文或包索引。

use std::{
    collections::{BinaryHeap, HashMap},
    io,
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE, RETRY_AFTER},
    },
    response::Response,
    routing::get,
};
use bytes::Bytes;
use capture_core::{
    BodyHandleMeta, BodyReadLease, CaptureError, MessageSide, ResponseRangeCandidate,
    responseContentRange, strongResponseEntityTag,
};
use futures_util::stream;
use parking_lot::Mutex;

use super::{ControlState, httpControl::LocalizedApiError, httpControl::mapCaptureLookupError};
use crate::localization::RequestLocale;

const previewStatusHeader: HeaderName = HeaderName::from_static("x-media-preview-status");
const previewCapturedBytesHeader: HeaderName =
    HeaderName::from_static("x-media-preview-captured-bytes");
const previewTotalBytesHeader: HeaderName = HeaderName::from_static("x-media-preview-total-bytes");
const previewSegmentCountHeader: HeaderName =
    HeaderName::from_static("x-media-preview-segment-count");
const mediaStreamChunkBytes: usize = 256 * 1024;
const overlapVerificationBytes: usize = 64;
const maximumIsoBoxCount: usize = 4_096;
const maximumIsoBoxDepth: usize = 8;
const maximumActivePreviewResponses: usize = 16;
const maximumPinnedPreviewBytes: usize = 1024 * 1024 * 1024;
const maximumActivePreviewHandles: usize = 64;

/// 保存单个正文实体在所有活动预览中的共享引用数；bytes 只在首个引用进入预算。
#[derive(Clone, Copy, Debug)]
struct PinnedPreviewEntity {
    bytes: usize,
    references: usize,
}

/// 保存当前控制会话的预览资源占用；所有字段只在极短同步临界区内更新。
#[derive(Debug, Default)]
struct MediaPreviewLeaseUsage {
    activeResponses: usize,
    activeHandles: usize,
    pinnedBytes: usize,
    entities: HashMap<String, PinnedPreviewEntity>,
}

/// 为控制会话限制慢媒体响应可固定的旧正文、句柄与并发数，避免 clear 后资源游离于录制预算。
#[derive(Clone, Debug, Default)]
pub(super) struct MediaPreviewLeaseBudget {
    usage: Arc<Mutex<MediaPreviewLeaseUsage>>,
}

/// 持有一次媒体响应的预算份额；随 Axum Body drop 自动释放，不允许手工提前归还。
#[derive(Debug)]
struct MediaPreviewLeaseReservation {
    budget: MediaPreviewLeaseBudget,
    entityIds: Vec<String>,
    handleCount: usize,
}

impl MediaPreviewLeaseBudget {
    /// 在打开正文租约前原子预留资源；同一响应内重复实体只计一次，跨响应正文 bytes 共享计费。
    ///
    /// `entities` 包含最终规划正文 ID 与完整存储长度。并发、句柄或唯一正文总量超过硬上限
    /// 返回空值，调用方必须在发送响应头前返回 429，绝不建立部分租约或中途截断。
    fn reserve(
        &self,
        entities: impl IntoIterator<Item = (String, usize)>,
    ) -> Option<MediaPreviewLeaseReservation> {
        let mut uniqueEntities = HashMap::new();
        for (identifier, bytes) in entities {
            uniqueEntities.entry(identifier).or_insert(bytes);
        }
        let handleCount = uniqueEntities.len();
        let mut usage = self.usage.lock();
        let additionalBytes = uniqueEntities
            .iter()
            .filter(|(identifier, _)| !usage.entities.contains_key(*identifier))
            .try_fold(0_usize, |total, (_, bytes)| total.checked_add(*bytes))?;
        if usage.activeResponses >= maximumActivePreviewResponses
            || usage
                .activeHandles
                .checked_add(handleCount)
                .is_none_or(|handles| handles > maximumActivePreviewHandles)
            || usage
                .pinnedBytes
                .checked_add(additionalBytes)
                .is_none_or(|bytes| bytes > maximumPinnedPreviewBytes)
        {
            return None;
        }
        usage.activeResponses += 1;
        usage.activeHandles += handleCount;
        usage.pinnedBytes += additionalBytes;
        for (identifier, bytes) in &uniqueEntities {
            usage
                .entities
                .entry(identifier.clone())
                .and_modify(|entity| entity.references += 1)
                .or_insert(PinnedPreviewEntity {
                    bytes: *bytes,
                    references: 1,
                });
        }
        Some(MediaPreviewLeaseReservation {
            budget: self.clone(),
            entityIds: uniqueEntities.into_keys().collect(),
            handleCount,
        })
    }
}

impl Drop for MediaPreviewLeaseReservation {
    /// 在响应结束、取消或 HEAD 返回时归还完整份额；最后一个实体引用负责释放 pinnedBytes。
    fn drop(&mut self) {
        let mut usage = self.budget.usage.lock();
        usage.activeResponses = usage
            .activeResponses
            .checked_sub(1)
            .expect("mediaPreviewActiveResponseAccountingMismatch");
        usage.activeHandles = usage
            .activeHandles
            .checked_sub(self.handleCount)
            .expect("mediaPreviewHandleAccountingMismatch");
        for identifier in &self.entityIds {
            let shouldRemove = {
                let entity = usage
                    .entities
                    .get_mut(identifier)
                    .expect("mediaPreviewEntityAccountingMismatch");
                entity.references = entity
                    .references
                    .checked_sub(1)
                    .expect("mediaPreviewEntityReferenceAccountingMismatch");
                entity.references == 0
            };
            if shouldRemove {
                let entity = usage
                    .entities
                    .remove(identifier)
                    .expect("mediaPreviewEntityRemovalAccountingMismatch");
                usage.pinnedBytes = usage
                    .pinnedBytes
                    .checked_sub(entity.bytes)
                    .expect("mediaPreviewPinnedByteAccountingMismatch");
            }
        }
    }
}

/// 描述虚拟媒体正文是否完整，前端只依据该枚举决定可播放状态和提示语。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MediaPreviewStatus {
    Complete,
    ContinuousPrefix,
    Incomplete,
}

impl MediaPreviewStatus {
    /// 返回公开响应头使用的稳定小写状态；该值由前端严格枚举校验。
    const fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ContinuousPrefix => "continuousPrefix",
            Self::Incomplete => "incomplete",
        }
    }
}

/// 汇总一个预览响应的媒体类型、稳定分段清单和声明长度。
#[derive(Debug)]
struct MediaPreviewBody {
    status: MediaPreviewStatus,
    contentType: String,
    segments: Vec<PreviewSegment>,
    capturedBytes: usize,
    totalBytes: u64,
    reservation: Option<MediaPreviewLeaseReservation>,
}

/// 映射虚拟媒体区间到稳定正文租约中的连续区间。
#[derive(Clone, Debug)]
struct PreviewSegment {
    lease: BodyReadLease,
    bodyOffset: usize,
    length: usize,
}

/// 保存惰性响应当前读取位置；该状态随 Axum Body 存活并持有所有正文租约。
struct PreviewStreamState {
    segments: Vec<PreviewSegment>,
    segmentIndex: usize,
    segmentOffset: usize,
    remainingBytes: usize,
    _reservation: MediaPreviewLeaseReservation,
}

/// 保存 HEAD/GET 与 Range 解析前的请求属性，确保两种方法共享同一响应契约。
#[derive(Clone, Debug)]
struct PreviewRequest {
    omitBody: bool,
    rangeHeader: Result<Option<String>, ()>,
}

/// 表示浏览器请求在虚拟媒体正文中的单一闭区间窗口。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreviewWindow {
    offset: usize,
    length: usize,
    end: usize,
}

/// 保存仅由二级索引元数据规划出的虚拟分段；正文租约要在最终清单确定后统一建立。
#[derive(Clone, Debug)]
struct PlannedRangeSegment {
    transactionId: String,
    body: BodyHandleMeta,
    bodyOffset: usize,
    length: usize,
}

/// 保存结构化 ISO BMFF 解析得到的容器与轨道证据。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IsoTrackKinds {
    isIsoBmff: bool,
    hasAudio: bool,
    hasVideo: bool,
}

/// 保存已校验 ISO BMFF 盒头边界，后续解析不得越过 boxEnd 裸搜索载荷。
#[derive(Clone, Copy, Debug)]
struct IsoBoxHeader {
    boxType: [u8; 4],
    payloadStart: u64,
    boxEnd: u64,
}

/// 注册媒体预览专用只读端点；路由独立于原始正文端点，避免调用者误以为录制内容已合并。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router.route(
        "/api/v1/transactions/{transactionId}/response/media-preview",
        get(getResponseMediaPreview),
    )
}

/// 构造媒体租约预算耗尽响应；客户端稍后重试即可，不会收到已经声明但中途截断的正文。
fn previewCapacityExceeded() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// 仅依据二级索引元数据建立零起点、去重叠的最大连续清单，不打开或读取任何正文。
///
/// 每轮用最大堆选择所有可达候选中结束位置最远、序号最新的事务；重叠候选只规划未覆盖
/// suffix。算法为 O(k log k)，其中 k 是同一实体版本的候选数，与全会话事务总量无关。
fn buildMetadataPlan(
    mut candidates: Vec<ResponseRangeCandidate>,
    total: u64,
) -> (Vec<PlannedRangeSegment>, usize) {
    candidates.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
            .then_with(|| right.sequence.cmp(&left.sequence))
    });
    let mut plan = Vec::new();
    let mut capturedBytes = 0_usize;
    let mut candidateIndex = 0_usize;
    while (capturedBytes as u64) < total {
        let mut reachable = BinaryHeap::new();
        while candidateIndex < candidates.len()
            && candidates[candidateIndex].start <= capturedBytes as u64
        {
            let candidate = &candidates[candidateIndex];
            reachable.push((candidate.end, candidate.sequence, candidateIndex));
            candidateIndex += 1;
        }
        let mut selected = None;
        while let Some((end, _, index)) = reachable.pop() {
            if end < capturedBytes as u64 {
                continue;
            }
            selected = Some(&candidates[index]);
            break;
        }
        let Some(candidate) = selected else {
            break;
        };
        let Ok(candidateStart) = usize::try_from(candidate.start) else {
            break;
        };
        let Some(bodyOffset) = capturedBytes.checked_sub(candidateStart) else {
            break;
        };
        let Some(segmentBytes) = candidate.end.checked_sub(capturedBytes as u64) else {
            break;
        };
        let Ok(length) = usize::try_from(segmentBytes + 1) else {
            break;
        };
        let Some(nextCapturedBytes) = capturedBytes.checked_add(length) else {
            break;
        };
        plan.push(PlannedRangeSegment {
            transactionId: candidate.transactionId.clone(),
            body: candidate.body.clone(),
            bodyOffset,
            length,
        });
        capturedBytes = nextCapturedBytes;
    }
    (plan, capturedBytes)
}

/// 按最终元数据清单中的唯一正文申请控制会话预算；任一算术或预算边界失败均返回空值。
fn reservePlannedSegments(
    state: &ControlState,
    plannedSegments: &[PlannedRangeSegment],
) -> Option<MediaPreviewLeaseReservation> {
    state.mediaPreviewLeaseBudget.reserve(
        plannedSegments
            .iter()
            .map(|segment| (segment.transactionId.clone(), segment.body.storedBytes)),
    )
}

/// 只为元数据规划最终选中的事务建立租约，并按固定窗口验证合法重叠的接缝字节。
///
/// 所有租约在 RecordingSession 的一个读锁内打开；clear/FIFO 若先完成则整批返回精确错误。
/// 接缝不一致时只保留此前已验证连续前缀，绝不尝试拼出损坏实体。
async fn leasePlannedSegments(
    state: &ControlState,
    plannedSegments: Vec<PlannedRangeSegment>,
) -> Result<(Vec<PreviewSegment>, usize), CaptureError> {
    let transactionIds = plannedSegments
        .iter()
        .map(|segment| segment.transactionId.clone())
        .collect::<Vec<_>>();
    let leases = state
        .recording
        .getBodyReadLeases(&transactionIds, MessageSide::Response)
        .await?;
    let mut segments = Vec::with_capacity(plannedSegments.len());
    let mut capturedBytes = 0_usize;
    for (planned, lease) in plannedSegments.into_iter().zip(leases) {
        if lease.meta() != &planned.body {
            return Err(CaptureError::BodyNotFound);
        }
        let verificationBytes = overlapVerificationBytes.min(planned.bodyOffset);
        if verificationBytes > 0 {
            let verificationStart = capturedBytes.saturating_sub(verificationBytes);
            let expected = readVirtualBytes(&segments, verificationStart as u64, verificationBytes)
                .await
                .ok_or(CaptureError::BodyNotFound)?;
            let actual = lease
                .readRange(planned.bodyOffset - verificationBytes, verificationBytes)
                .await?;
            if actual != expected {
                break;
            }
        }
        capturedBytes = capturedBytes
            .checked_add(planned.length)
            .ok_or(CaptureError::InvalidBodyLength)?;
        segments.push(PreviewSegment {
            lease,
            bodyOffset: planned.bodyOffset,
            length: planned.length,
        });
    }
    Ok((segments, capturedBytes))
}

/// 从虚拟连续正文读取一个很小的随机访问区间，供 ISO BMFF 盒解析器检查头部。
///
/// 运行上下文：解析器每次最多请求 16 字节，本函数可跨事务边界拼接该有界窗口；不会读取
/// `mdat` 负载或聚合媒体。正文元信息变化、缺失或短读时返回空值，类型识别随即停止。
async fn readVirtualBytes(
    segments: &[PreviewSegment],
    offset: u64,
    length: usize,
) -> Option<Vec<u8>> {
    let mut virtualStart = 0_u64;
    let mut remainingOffset = offset;
    let mut bytes = Vec::with_capacity(length);
    for segment in segments {
        let segmentLength = segment.length as u64;
        let virtualEnd = virtualStart.checked_add(segmentLength)?;
        if remainingOffset >= virtualEnd {
            virtualStart = virtualEnd;
            continue;
        }
        let segmentOffset = usize::try_from(remainingOffset.checked_sub(virtualStart)?).ok()?;
        let bodyOffset = segment.bodyOffset.checked_add(segmentOffset)?;
        let readBytes = (length - bytes.len()).min(segment.length - segmentOffset);
        let response = segment.lease.readRange(bodyOffset, readBytes).await.ok()?;
        if response.len() != readBytes {
            return None;
        }
        bytes.extend_from_slice(&response);
        if bytes.len() == length {
            return Some(bytes);
        }
        virtualStart = virtualEnd;
        remainingOffset = virtualEnd;
    }
    None
}

/// 读取并校验单个 ISO BMFF 盒头，支持 32 位、64 位 extended size 与 size=0。
///
/// `regionEnd` 是父容器的硬边界；任何盒尺寸越界、头部不完整或算术溢出都返回空值，
/// 防止在 `mdat` 负载中继续裸搜索伪造的 `hdlr` 字节。
async fn readIsoBoxHeader(
    segments: &[PreviewSegment],
    boxStart: u64,
    regionEnd: u64,
) -> Option<IsoBoxHeader> {
    if regionEnd.checked_sub(boxStart)? < 8 {
        return None;
    }
    let basicHeader = readVirtualBytes(segments, boxStart, 8).await?;
    let size32 = u32::from_be_bytes(basicHeader[..4].try_into().ok()?);
    let boxType = basicHeader[4..8].try_into().ok()?;
    let (boxSize, headerBytes) = match size32 {
        0 => (regionEnd.checked_sub(boxStart)?, 8_u64),
        1 => {
            if regionEnd.checked_sub(boxStart)? < 16 {
                return None;
            }
            let extendedSizeBytes = readVirtualBytes(segments, boxStart + 8, 8).await?;
            (
                u64::from_be_bytes(extendedSizeBytes[..8].try_into().ok()?),
                16,
            )
        }
        size => (u64::from(size), 8),
    };
    if boxSize < headerBytes {
        return None;
    }
    let boxEnd = boxStart.checked_add(boxSize)?;
    (boxEnd <= regionEnd).then_some(IsoBoxHeader {
        boxType,
        payloadStart: boxStart + headerBytes,
        boxEnd,
    })
}

/// 按 ISO BMFF 盒边界识别媒体轨道；解析可跳过大型 mdat 并定位文件尾部 moov。
///
/// 运行上下文：只递归 `moov/trak/mdia` 必要容器，在 `mdia` 的直接 `hdlr` 子盒读取
/// handler_type；最大深度和盒数量均有硬上限。未知、损坏或尚未捕获完整的盒返回已有证据，
/// 不使用扩展名、裸字节搜索或错误 Content-Type 猜测轨道类型。
async fn inspectIsoTracks(segments: &[PreviewSegment], capturedBytes: usize) -> IsoTrackKinds {
    let totalBytes = capturedBytes as u64;
    let Some(firstBox) = readIsoBoxHeader(segments, 0, totalBytes).await else {
        return IsoTrackKinds::default();
    };
    let mut tracks = IsoTrackKinds {
        isIsoBmff: matches!(&firstBox.boxType, b"ftyp" | b"styp" | b"moov"),
        ..IsoTrackKinds::default()
    };
    if !tracks.isIsoBmff {
        return tracks;
    }
    let mut regions = vec![(0_u64, totalBytes, 0_usize, false)];
    let mut inspectedBoxes = 0_usize;
    while let Some((mut cursor, regionEnd, depth, insideMedia)) = regions.pop() {
        while cursor < regionEnd && inspectedBoxes < maximumIsoBoxCount {
            let Some(header) = readIsoBoxHeader(segments, cursor, regionEnd).await else {
                break;
            };
            inspectedBoxes += 1;
            if insideMedia
                && header.boxType == *b"hdlr"
                && let Some(handler) = readVirtualBytes(segments, header.payloadStart, 12).await
            {
                tracks.hasAudio |= &handler[8..12] == b"soun";
                tracks.hasVideo |= &handler[8..12] == b"vide";
            }
            let isContainer = matches!(&header.boxType, b"moov" | b"trak" | b"mdia");
            if isContainer && depth < maximumIsoBoxDepth && header.payloadStart < header.boxEnd {
                if header.boxEnd < regionEnd {
                    regions.push((header.boxEnd, regionEnd, depth, insideMedia));
                }
                regions.push((
                    header.payloadStart,
                    header.boxEnd,
                    depth + 1,
                    insideMedia || header.boxType == *b"mdia",
                ));
                break;
            }
            cursor = header.boxEnd;
        }
    }
    tracks
}

/// 依据结构化容器证据纠正媒体 MIME；未知轨道的 MP4 使用中性类型，避免选择错误解码器。
async fn effectiveContentType(
    declared: &str,
    segments: &[PreviewSegment],
    capturedBytes: usize,
) -> String {
    let tracks = inspectIsoTracks(segments, capturedBytes).await;
    if !tracks.isIsoBmff {
        return declared.to_owned();
    }
    if tracks.hasVideo {
        return "video/mp4".to_owned();
    }
    if tracks.hasAudio {
        return "audio/mp4".to_owned();
    }
    "application/mp4".to_owned()
}

/// 在下游每次轮询时读取恰好一个正文分块；未被轮询时不会启动磁盘读取或预取下一块。
///
/// FIFO 淘汰与 clear 只移除事务和路径，状态持有的稳定租约会继续读取同一正文代际。
/// 租约句柄 I/O 失败、偏移溢出或短读时返回一次错误并中断响应，禁止把缺失尾部伪装成
/// 成功媒体；任一时刻仅分配一个固定上限分块。
async fn readNextPreviewChunk(
    mut state: PreviewStreamState,
) -> Option<(Result<Bytes, io::Error>, PreviewStreamState)> {
    if state.remainingBytes == 0 {
        return None;
    }
    while state.segmentIndex < state.segments.len() {
        let segment = &state.segments[state.segmentIndex];
        if state.segmentOffset == segment.length {
            state.segmentIndex += 1;
            state.segmentOffset = 0;
            continue;
        }
        let maximumBytes = mediaStreamChunkBytes
            .min(segment.length - state.segmentOffset)
            .min(state.remainingBytes);
        let Some(bodyOffset) = segment.bodyOffset.checked_add(state.segmentOffset) else {
            state.remainingBytes = 0;
            return Some((
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mediaPreviewOffsetOverflow",
                )),
                state,
            ));
        };
        let response = match segment.lease.readRange(bodyOffset, maximumBytes).await {
            Ok(response) if response.len() == maximumBytes => response,
            Ok(_) => {
                state.segmentIndex = state.segments.len();
                return Some((
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "mediaPreviewBodyChanged",
                    )),
                    state,
                ));
            }
            Err(error) => {
                state.segmentIndex = state.segments.len();
                return Some((Err(io::Error::other(error.to_string())), state));
            }
        };
        state.segmentOffset += response.len();
        state.remainingBytes -= response.len();
        return Some((Ok(Bytes::from(response)), state));
    }
    // 清单长度在响应头生成前已经验证；若内部清单意外提前结束，必须让 Hyper 中断传输，
    // 不能在固定 Content-Length 尚未满足时返回 EOF 并把损坏媒体伪装成成功响应。
    state.remainingBytes = 0;
    Some((
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "mediaPreviewPlanEndedEarly",
        )),
        state,
    ))
}

/// 创建由 HTTP Body poll 驱动的窗口化惰性媒体流；支持从虚拟正文任意偏移开始且严格限长。
///
/// Range 偏移会在构造阶段映射到对应元数据段，不读取正文；越界窗口由上层在调用前拒绝。
/// segments 持有的租约随响应 Body 一同存活并在 Body drop 时释放，clear 不参与该资源生命周期。
fn streamPreviewBody(
    segments: Vec<PreviewSegment>,
    window: PreviewWindow,
    reservation: MediaPreviewLeaseReservation,
) -> Body {
    let mut skippedBytes = 0_usize;
    let mut segmentIndex = 0_usize;
    while segmentIndex < segments.len()
        && skippedBytes + segments[segmentIndex].length <= window.offset
    {
        skippedBytes += segments[segmentIndex].length;
        segmentIndex += 1;
    }
    Body::from_stream(stream::unfold(
        PreviewStreamState {
            segments,
            segmentIndex,
            segmentOffset: window.offset - skippedBytes,
            remainingBytes: window.length,
            _reservation: reservation,
        },
        readNextPreviewChunk,
    ))
}

/// 从请求头提取唯一 Range 字段；重复字段、非 ASCII 字段与多范围列表均视为不可满足。
///
/// 字段语法要等虚拟正文长度确定后再解析，因此此处保留原值。无 Range 返回 Ok(None)。
fn requestedRange(headers: &HeaderMap) -> Result<Option<String>, ()> {
    let mut values = headers.get_all(RANGE).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = first.to_str().map_err(|_| ())?.trim();
    if value.contains(',') {
        return Err(());
    }
    Ok(Some(value.to_owned()))
}

/// 将单个 HTTP byte-range 解析为虚拟正文窗口；支持闭区间、开放尾端和 suffix 三种形式。
///
/// 结束位置超过正文时按 RFC 截到末尾；空正文、零 suffix、倒置区间、起点越界和非 bytes
/// 单位返回错误。多范围在 requestedRange 阶段已被拒绝，避免 multipart 聚合与额外内存。
fn resolvePreviewWindow(range: Option<&str>, resourceBytes: usize) -> Result<PreviewWindow, ()> {
    let Some(value) = range else {
        return Ok(PreviewWindow {
            offset: 0,
            length: resourceBytes,
            end: resourceBytes.saturating_sub(1),
        });
    };
    if resourceBytes == 0 {
        return Err(());
    }
    let (unit, bounds) = value.split_once('=').ok_or(())?;
    if !unit.eq_ignore_ascii_case("bytes") || bounds.is_empty() {
        return Err(());
    }
    let (startText, endText) = bounds.split_once('-').ok_or(())?;
    let (offset, end) = if startText.is_empty() {
        let suffixBytes = endText.parse::<usize>().map_err(|_| ())?;
        if suffixBytes == 0 {
            return Err(());
        }
        let length = suffixBytes.min(resourceBytes);
        (resourceBytes - length, resourceBytes - 1)
    } else {
        let start = startText.parse::<usize>().map_err(|_| ())?;
        if start >= resourceBytes {
            return Err(());
        }
        let end = if endText.is_empty() {
            resourceBytes - 1
        } else {
            endText
                .parse::<usize>()
                .map_err(|_| ())?
                .min(resourceBytes - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(PreviewWindow {
        offset,
        length: end - offset + 1,
        end,
    })
}

/// 构造 416 响应并公开当前虚拟正文长度；浏览器可据此重新发起合法的单范围请求。
fn rangeNotSatisfiable(preview: &MediaPreviewBody) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    headers.insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{}", preview.capturedBytes))
            .expect("无符号整数必须是有效 Content-Range"),
    );
    response
}

/// 把预览元数据写入响应头，并按可选单 Range 将分段清单转换为惰性 Axum 二进制流。
fn intoResponse(preview: MediaPreviewBody, request: PreviewRequest) -> Response {
    let segmentCount = preview.segments.len();
    let rangeHeader = match request.rangeHeader {
        Ok(rangeHeader) => rangeHeader,
        Err(()) => return rangeNotSatisfiable(&preview),
    };
    let window = match resolvePreviewWindow(rangeHeader.as_deref(), preview.capturedBytes) {
        Ok(window) => window,
        Err(()) => return rangeNotSatisfiable(&preview),
    };
    let isPartial = rangeHeader.is_some();
    let body = if request.omitBody {
        Body::empty()
    } else if let Some(reservation) = preview.reservation {
        streamPreviewBody(preview.segments, window, reservation)
    } else {
        Body::empty()
    };
    let mut response = Response::new(body);
    *response.status_mut() = if isPartial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if isPartial {
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                window.offset, window.end, preview.capturedBytes
            ))
            .expect("已校验范围必须是有效 Content-Range"),
        );
    }
    headers.insert(
        previewStatusHeader,
        HeaderValue::from_static(preview.status.label()),
    );
    for (name, value) in [
        (previewCapturedBytesHeader, preview.capturedBytes as u64),
        (previewTotalBytesHeader, preview.totalBytes),
        (previewSegmentCountHeader, segmentCount as u64),
        (CONTENT_LENGTH, window.length as u64),
    ] {
        headers.insert(
            name,
            HeaderValue::from_str(&value.to_string()).expect("无符号整数必须是有效响应头"),
        );
    }
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&preview.contentType)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response
}

/// 返回选中响应的可播放虚拟正文；原始正文端点继续逐事务返回未改写的完整字节。
async fn getResponseMediaPreview(
    method: Method,
    headers: HeaderMap,
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(transactionId): Path<String>,
) -> Result<Response, LocalizedApiError> {
    let previewRequest = PreviewRequest {
        omitBody: method == Method::HEAD,
        rangeHeader: requestedRange(&headers),
    };
    let selected = state
        .recording
        .getTransactionDetail(&transactionId)
        .await
        .map_err(mapCaptureLookupError)
        .map_err(|error| error.withLocale(locale))?;
    let selectedBody = selected.responseBody.clone().ok_or_else(|| {
        mapCaptureLookupError(capture_core::CaptureError::BodyNotFound).withLocale(locale)
    })?;
    let selectedRange = responseContentRange(&selected.responseHeaders);
    // 206 的区间边界只能来自唯一且合法的 Content-Range。重复或畸形字段不是普通完整响应，
    // 若继续走 200 正文分支会把局部分段错误标记为 complete，并向浏览器声明错误长度。
    if selected.transaction.statusCode == Some(206) && selectedRange.is_none() {
        let originalBytes = selectedBody.originalBytes;
        return Ok(intoResponse(
            incompleteBody(selectedBody, originalBytes),
            previewRequest,
        ));
    }
    let Some((selectedStart, selectedEnd, totalBytes)) = selectedRange else {
        if !selectedBody.encoding.is_empty()
            && !selectedBody.encoding.eq_ignore_ascii_case("identity")
        {
            return Ok(intoResponse(
                incompleteBody(selectedBody.clone(), selectedBody.originalBytes),
                previewRequest,
            ));
        }
        let Some(reservation) = state
            .mediaPreviewLeaseBudget
            .reserve([(transactionId.clone(), selectedBody.storedBytes)])
        else {
            return Ok(previewCapacityExceeded());
        };
        let lease = state
            .recording
            .getBodyReadLease(&transactionId, MessageSide::Response)
            .await
            .map_err(mapCaptureLookupError)
            .map_err(|error| error.withLocale(locale))?;
        if lease.meta() != &selectedBody {
            return Err(mapCaptureLookupError(CaptureError::BodyNotFound).withLocale(locale));
        }
        let segment = PreviewSegment {
            lease,
            bodyOffset: 0,
            length: selectedBody.storedBytes,
        };
        let segments = vec![segment];
        let contentType = effectiveContentType(
            &selectedBody.contentType,
            &segments,
            selectedBody.storedBytes,
        )
        .await;
        return Ok(intoResponse(
            MediaPreviewBody {
                status: if selectedBody.truncated {
                    MediaPreviewStatus::ContinuousPrefix
                } else {
                    MediaPreviewStatus::Complete
                },
                contentType,
                segments,
                capturedBytes: selectedBody.storedBytes,
                totalBytes: selectedBody.originalBytes,
                reservation: Some(reservation),
            },
            previewRequest,
        ));
    };
    if !selectedBody.encoding.is_empty() && !selectedBody.encoding.eq_ignore_ascii_case("identity")
    {
        return Ok(intoResponse(
            incompleteBody(selectedBody, totalBytes),
            previewRequest,
        ));
    }

    let (segments, capturedBytes, reservation) =
        if let Some(selectedEntityTag) = strongResponseEntityTag(&selected.responseHeaders) {
            let candidates = state
                .recording
                .findResponseRangeCandidates(
                    &selected.transaction.urlDisplay,
                    selectedEntityTag,
                    totalBytes,
                    &selectedBody.encoding,
                )
                .await
                .map_err(mapCaptureLookupError)
                .map_err(|error| error.withLocale(locale))?;
            let (plannedSegments, _) = buildMetadataPlan(candidates, totalBytes);
            if plannedSegments.is_empty() {
                return Ok(intoResponse(
                    incompleteBody(selectedBody, totalBytes),
                    previewRequest,
                ));
            }
            let Some(reservation) = reservePlannedSegments(&state, &plannedSegments) else {
                return Ok(previewCapacityExceeded());
            };
            let (segments, capturedBytes) = leasePlannedSegments(&state, plannedSegments)
                .await
                .map_err(mapCaptureLookupError)
                .map_err(|error| error.withLocale(locale))?;
            (segments, capturedBytes, reservation)
        } else {
            let selectedRangeBytes = selectedEnd - selectedStart + 1;
            if selectedStart != 0
                || selectedBody.truncated
                || selectedBody.originalBytes != selectedRangeBytes
                || selectedBody.storedBytes as u64 != selectedRangeBytes
            {
                return Ok(intoResponse(
                    incompleteBody(selectedBody, totalBytes),
                    previewRequest,
                ));
            }
            let Some(reservation) = state
                .mediaPreviewLeaseBudget
                .reserve([(transactionId.clone(), selectedBody.storedBytes)])
            else {
                return Ok(previewCapacityExceeded());
            };
            let lease = state
                .recording
                .getBodyReadLease(&transactionId, MessageSide::Response)
                .await
                .map_err(mapCaptureLookupError)
                .map_err(|error| error.withLocale(locale))?;
            if lease.meta() != &selectedBody {
                return Err(mapCaptureLookupError(CaptureError::BodyNotFound).withLocale(locale));
            }
            let segment = PreviewSegment {
                lease,
                bodyOffset: 0,
                length: selectedBody.storedBytes,
            };
            (vec![segment], selectedBody.storedBytes, reservation)
        };
    if segments.is_empty() {
        return Ok(intoResponse(
            incompleteBody(selectedBody, totalBytes),
            previewRequest,
        ));
    }
    let status = if capturedBytes as u64 == totalBytes {
        MediaPreviewStatus::Complete
    } else {
        MediaPreviewStatus::ContinuousPrefix
    };
    let contentType =
        effectiveContentType(&selectedBody.contentType, &segments, capturedBytes).await;
    Ok(intoResponse(
        MediaPreviewBody {
            status,
            contentType,
            segments,
            capturedBytes,
            totalBytes,
            reservation: Some(reservation),
        },
        previewRequest,
    ))
}

/// 构造缺少起始片段或可靠实体代际时的显式空状态，不返回不可解码的伪正文。
fn incompleteBody(body: BodyHandleMeta, totalBytes: u64) -> MediaPreviewBody {
    MediaPreviewBody {
        status: MediaPreviewStatus::Incomplete,
        contentType: body.contentType,
        segments: Vec::new(),
        capturedBytes: 0,
        totalBytes,
        reservation: None,
    }
}
