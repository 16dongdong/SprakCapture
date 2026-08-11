//! 提供与具体协议无关的分包、合包和长度前缀重封包帮助器。

use std::fmt;

/// 描述分包和帧解析失败；调用方收到错误后应保留原始线上字节。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PacketError {
    ZeroPacketSize,
    PacketTooLarge { length: usize, maximum: usize },
    LengthOverflow,
}

impl fmt::Display for PacketError {
    /// 返回适合插件日志的中文错误；不包含正文内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPacketSize => formatter.write_str("分包大小必须大于零"),
            Self::PacketTooLarge { length, maximum } => {
                write!(formatter, "帧长度 {length} 超过上限 {maximum}")
            }
            Self::LengthOverflow => formatter.write_str("合包总长度溢出地址空间"),
        }
    }
}

impl std::error::Error for PacketError {}

/// 按最大负载长度切分 TCP 输出或 UDP 应用层片段；空负载保留为一个空包。
pub fn splitPackets(bytes: &[u8], maximumPacketBytes: usize) -> Result<Vec<Vec<u8>>, PacketError> {
    if maximumPacketBytes == 0 {
        return Err(PacketError::ZeroPacketSize);
    }
    if bytes.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    Ok(bytes
        .chunks(maximumPacketBytes)
        .map(<[u8]>::to_vec)
        .collect())
}

/// 按原顺序合并分片；在分配前检查总长度，失败时不产生部分输出。
pub fn joinPackets<P>(packets: P) -> Result<Vec<u8>, PacketError>
where
    P: IntoIterator,
    P::Item: AsRef<[u8]>,
{
    let packets = packets.into_iter().collect::<Vec<_>>();
    let totalBytes = packets.iter().try_fold(0_usize, |total, packet| {
        total
            .checked_add(packet.as_ref().len())
            .ok_or(PacketError::LengthOverflow)
    })?;
    let mut joined = Vec::with_capacity(totalBytes);
    for packet in packets {
        joined.extend_from_slice(packet.as_ref());
    }
    Ok(joined)
}

/// 对跨多个 TCP chunk 的 4 字节大端长度前缀帧进行增量拆包和重封包。
#[derive(Clone, Debug)]
pub struct LengthPrefixedFrames {
    buffered: Vec<u8>,
    maximumFrameBytes: usize,
}

impl LengthPrefixedFrames {
    /// 创建增量解析器；零上限没有可表达帧，直接返回错误。
    pub fn new(maximumFrameBytes: usize) -> Result<Self, PacketError> {
        if maximumFrameBytes == 0 {
            return Err(PacketError::ZeroPacketSize);
        }
        Ok(Self {
            buffered: Vec::new(),
            maximumFrameBytes,
        })
    }

    /// 追加一个 TCP chunk 并返回当前所有完整帧；不完整尾部保留到下一次调用。
    ///
    /// 声明长度超过上限时返回错误且清空缓冲，防止攻击性前缀永久占用连接内存。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        self.buffered.extend_from_slice(chunk);
        let mut frames = Vec::new();
        let mut consumed = 0_usize;
        while self.buffered.len().saturating_sub(consumed) >= 4 {
            let length = u32::from_be_bytes(
                self.buffered[consumed..consumed + 4]
                    .try_into()
                    .expect("已验证长度前缀边界"),
            ) as usize;
            if length > self.maximumFrameBytes {
                self.buffered.clear();
                return Err(PacketError::PacketTooLarge {
                    length,
                    maximum: self.maximumFrameBytes,
                });
            }
            let frameEnd = consumed
                .checked_add(4)
                .and_then(|offset| offset.checked_add(length))
                .ok_or(PacketError::LengthOverflow)?;
            if frameEnd > self.buffered.len() {
                break;
            }
            frames.push(self.buffered[consumed + 4..frameEnd].to_vec());
            consumed = frameEnd;
        }
        if consumed > 0 {
            self.buffered.drain(..consumed);
        }
        Ok(frames)
    }

    /// 用 4 字节大端长度前缀重封包单帧；超出配置或 u32 上限时拒绝输出。
    pub fn encode(&self, frame: &[u8]) -> Result<Vec<u8>, PacketError> {
        if frame.len() > self.maximumFrameBytes || frame.len() > u32::MAX as usize {
            return Err(PacketError::PacketTooLarge {
                length: frame.len(),
                maximum: self.maximumFrameBytes.min(u32::MAX as usize),
            });
        }
        let mut packet = Vec::with_capacity(4 + frame.len());
        packet.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        packet.extend_from_slice(frame);
        Ok(packet)
    }

    /// 返回尚未形成完整帧的字节数，用于连接关闭时判断协议尾部是否完整。
    pub fn bufferedBytes(&self) -> usize {
        self.buffered.len()
    }
}
