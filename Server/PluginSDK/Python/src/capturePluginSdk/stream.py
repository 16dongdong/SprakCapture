"""实现连接级双向增量分包、变换与重封包组合。"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Protocol


@dataclass(frozen=True, slots=True)
class Frame:
    """保存一帧协议正文以及编解码器需要继续携带的元数据。"""

    payload: bytes
    metadata: dict[str, object]


class FrameCodec(Protocol):
    """定义增量分包与重封包契约；decode 不得消费半帧。"""

    def decode(self, bufferedBytes: bytes) -> tuple[Frame | None, int]: ...
    def encode(self, frame: Frame) -> bytes: ...


class PayloadCipher(Protocol):
    """定义可替换的解密与加密过程；实现可维护连接外部的密钥状态。"""

    def decrypt(self, payload: bytes) -> bytes: ...
    def encrypt(self, payload: bytes) -> bytes: ...


class IdentityCipher:
    """为无加密协议提供零配置密码器。"""

    def decrypt(self, payload: bytes) -> bytes:
        """原样返回不可变正文；不会创建不必要的第二份数据。"""
        return payload

    def encrypt(self, payload: bytes) -> bytes:
        """原样返回处理后的正文；用于与加密管线保持统一调用形式。"""
        return payload


class LengthPrefixedCodec:
    """处理固定宽度大端/小端长度前缀协议，长度默认包含正文而不包含前缀。"""

    def __init__(self, prefixBytes: int = 4, byteOrder: str = "big", includesPrefix: bool = False) -> None:
        """配置长度字段；非法宽度或字节序在启动时失败，避免在线上产生坏包。"""
        if prefixBytes < 1 or prefixBytes > 8 or byteOrder not in {"big", "little"}:
            raise ValueError("长度前缀必须为 1..8 字节且字节序有效")
        self.prefixBytes = prefixBytes
        self.byteOrder = byteOrder
        self.includesPrefix = includesPrefix

    def decode(self, bufferedBytes: bytes) -> tuple[Frame | None, int]:
        """从连续窗口提取一帧；数据不足时返回 (None, 0)，不会消费半帧。"""
        if len(bufferedBytes) < self.prefixBytes:
            return None, 0
        declaredLength = int.from_bytes(bufferedBytes[: self.prefixBytes], self.byteOrder)
        payloadLength = declaredLength - self.prefixBytes if self.includesPrefix else declaredLength
        if payloadLength < 0:
            raise ValueError("帧长度小于长度字段宽度")
        frameLength = self.prefixBytes + payloadLength
        if len(bufferedBytes) < frameLength:
            return None, 0
        return Frame(bufferedBytes[self.prefixBytes:frameLength], {}), frameLength

    def encode(self, frame: Frame) -> bytes:
        """重算长度并序列化完整帧；长度溢出前缀容量时明确失败。"""
        declaredLength = len(frame.payload) + (self.prefixBytes if self.includesPrefix else 0)
        try:
            prefix = declaredLength.to_bytes(self.prefixBytes, self.byteOrder)
        except OverflowError as error:
            raise ValueError("重封包后的正文超过长度字段容量") from error
        return prefix + frame.payload


class StreamPipeline:
    """按 connectionId+direction 隔离半包缓冲，并串联分包、解密、修改和重封包。"""

    def __init__(self, codecFactory: Callable[[str, str], FrameCodec], cipherFactory: Callable[[str, str], PayloadCipher] | None = None) -> None:
        """保存每个方向的状态工厂；工厂失败会终止当前调用且不会发布部分输出。"""
        self.codecFactory = codecFactory
        self.cipherFactory = cipherFactory or (lambda _connectionId, _direction: IdentityCipher())
        self.buffers: dict[tuple[str, str], bytearray] = {}
        self.codecs: dict[tuple[str, str], FrameCodec] = {}
        self.ciphers: dict[tuple[str, str], PayloadCipher] = {}

    def push(self, connectionId: str, direction: str, chunk: bytes, transform: Callable[[Frame], Frame | bytes | None]) -> bytes | None:
        """追加一个 TCP 块并尽可能提取全部完整帧；None 表示仍在等待半帧。"""
        key = (connectionId, direction)
        buffer = self.buffers.setdefault(key, bytearray())
        buffer.extend(chunk)
        codec = self.codecs.get(key)
        if codec is None:
            codec = self.codecFactory(connectionId, direction)
            self.codecs[key] = codec
        cipher = self.ciphers.get(key)
        if cipher is None:
            cipher = self.cipherFactory(connectionId, direction)
            self.ciphers[key] = cipher
        outputFrames: list[bytes] = []
        consumedFrame = False
        while buffer:
            frame, consumedBytes = codec.decode(bytes(buffer))
            if frame is None:
                break
            if consumedBytes < 1 or consumedBytes > len(buffer):
                raise ValueError("编解码器返回了无效消费长度")
            del buffer[:consumedBytes]
            consumedFrame = True
            clearFrame = Frame(cipher.decrypt(frame.payload), frame.metadata)
            changed = transform(clearFrame)
            if changed is None:
                continue
            changedFrame = changed if isinstance(changed, Frame) else Frame(bytes(changed), frame.metadata)
            outputFrames.append(codec.encode(Frame(cipher.encrypt(changedFrame.payload), changedFrame.metadata)))
        if outputFrames:
            return b"".join(outputFrames)
        return b"" if consumedFrame else None

    def close(self, connectionId: str) -> None:
        """清理连接两个方向的半包和密码状态；残留半帧会报告错误而非静默丢失。"""
        keys = [key for key in self.buffers if key[0] == connectionId]
        if any(self.buffers[key] for key in keys):
            raise ValueError("连接关闭时仍存在未完成帧")
        for key in keys:
            self.buffers.pop(key, None)
            self.codecs.pop(key, None)
            self.ciphers.pop(key, None)
