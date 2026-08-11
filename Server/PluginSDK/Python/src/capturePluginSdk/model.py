"""定义 Python 插件与宿主交换的稳定事件和动作模型。"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

PROCESS_JSON_SAFE_INTEGER_MAX = 9_007_199_254_740_991


class Stage(StrEnum):
    """列出 API 2.x 的公开阶段；枚举值直接对应宿主 JSON 线名。"""

    SERVICE_STARTING = "serviceStarting"
    SERVICE_STARTED = "serviceStarted"
    CONFIGURATION_CHANGED = "configurationChanged"
    SERVICE_STOPPING = "serviceStopping"
    CONNECTION_ACCEPTED = "connectionAccepted"
    SOCKS5_AUTHENTICATION = "socks5Authentication"
    PROTOCOL_CLASSIFIED = "protocolClassified"
    TARGET_RESOLVING = "targetResolving"
    BEFORE_CONNECT = "beforeConnect"
    CONNECTED = "connected"
    CONNECTION_CLOSING = "connectionClosing"
    CLIENT_HELLO_OBSERVED = "clientHelloObserved"
    CERTIFICATE_SELECTING = "certificateSelecting"
    TLS_ESTABLISHED = "tlsEstablished"
    TLS_FAILED = "tlsFailed"
    REQUEST_HEADERS = "requestHeaders"
    REQUEST_BODY_CHUNK = "requestBodyChunk"
    REQUEST_COMPLETE = "requestComplete"
    BEFORE_UPSTREAM = "beforeUpstream"
    RESPONSE_HEADERS = "responseHeaders"
    RESPONSE_BODY_CHUNK = "responseBodyChunk"
    RESPONSE_COMPLETE = "responseComplete"
    WEB_SOCKET_OPENING = "webSocketOpening"
    WEB_SOCKET_FRAME = "webSocketFrame"
    WEB_SOCKET_CLOSING = "webSocketClosing"
    TCP_CHUNK = "tcpChunk"
    UDP_DATAGRAM = "udpDatagram"
    DNS_MESSAGE = "dnsMessage"
    BEFORE_RECORD = "beforeRecord"
    TRANSACTION_UPDATED = "transactionUpdated"
    TRANSACTION_COMPLETED = "transactionCompleted"
    RECORDING_CLEARED = "recordingCleared"
    INSPECTOR_DATA_REQUESTED = "inspectorDataRequested"
    COMMAND_INVOKED = "commandInvoked"
    CONTEXT_ACTION_INVOKED = "contextActionInvoked"


class ActionKind(StrEnum):
    """表示插件对当前事件的决定；实际可用动作仍由阶段语义决定。"""

    CONTINUE = "continue"
    MODIFY = "modify"
    HOLD = "hold"
    DROP = "drop"
    REJECT = "reject"
    RESPOND = "respond"
    REDIRECT = "redirect"
    ANNOTATE = "annotate"
    CLOSE = "close"


@dataclass(frozen=True, slots=True)
class EventContext:
    """提供稳定匹配上下文；缺失字段保留为 None，避免伪造连接身份。"""

    raw: dict[str, Any]

    def value(self, name: str, default: Any = None) -> Any:
        """读取上下文字段；名称不存在时返回调用方给定默认值，不修改原始事件。"""
        return self.raw.get(name, default)

    @property
    def direction(self) -> str | None:
        """返回当前 TCP/UDP 数据方向；非数据面阶段返回 None。"""
        return self.raw.get("direction")


@dataclass(frozen=True, slots=True)
class Event:
    """封装一次不可变宿主调用，并提供正文与上下文的便捷访问。"""

    apiVersion: str
    eventId: str
    stage: Stage
    serviceGeneration: int
    recordingGeneration: int
    pluginInstanceId: str
    connectionId: str | None
    transactionId: str | None
    deadlineUnixMs: int
    context: EventContext
    payload: Any
    pluginId: str = ""
    moduleId: str = ""
    moduleKind: str = ""

    @classmethod
    def fromInvocation(cls, invocation: dict[str, Any]) -> "Event":
        """从 Native ABI 的 RuntimeInvocation JSON 创建强类型事件；字段错误会明确抛出 ValueError。"""
        try:
            envelope = invocation["envelope"]
            return cls(
                apiVersion=str(envelope["apiVersion"]),
                eventId=str(envelope["eventId"]),
                stage=Stage(envelope["stage"]),
                serviceGeneration=int(envelope["serviceGeneration"]),
                recordingGeneration=int(envelope["recordingGeneration"]),
                pluginInstanceId=str(envelope["pluginInstanceId"]),
                connectionId=envelope.get("connectionId"),
                transactionId=envelope.get("transactionId"),
                deadlineUnixMs=int(envelope["deadlineUnixMs"]),
                context=EventContext(dict(envelope["context"])),
                payload=envelope["payload"],
                pluginId=str(invocation.get("pluginId", "")),
                moduleId=str(invocation.get("moduleId", "")),
                moduleKind=str(invocation.get("moduleKind", "")),
            )
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"无效的插件调用：{error}") from error

    @property
    def bytes(self) -> bytes:
        """读取 TCP/UDP/正文事件的字节数组；遇到非整数或越界值立即报告协议错误。"""
        if not isinstance(self.payload, dict):
            raise ValueError("二进制事件 payload 必须是对象")
        values = self.payload.get("bytes", [])
        if not isinstance(values, list) or any(not isinstance(value, int) or value < 0 or value > 255 for value in values):
            raise ValueError("payload.bytes 必须是 0..255 的整数数组")
        return bytes(values)


@dataclass(frozen=True, slots=True)
class Action:
    """保存可直接序列化给宿主的标准动作。"""

    eventId: str
    action: ActionKind
    patch: list[dict[str, Any]] = field(default_factory=list)
    annotations: list[dict[str, Any]] = field(default_factory=list)
    output: Any = None

    def toDict(self) -> dict[str, Any]:
        """生成符合 API 2.x 的 camelCase JSON 对象；返回值不共享可变容器。"""
        return {
            "eventId": self.eventId,
            "action": self.action.value,
            "patch": list(self.patch),
            "annotations": list(self.annotations),
            "output": self.output,
        }
