"""提供函数调用式动作构造器，插件作者不需要手写 JSON Patch。"""

from __future__ import annotations

from typing import Any

from .model import Action, ActionKind, Event


def requireNonBlank(value: str, message: str) -> str:
    """校验终止原因和目标主机包含可见字符；纯空白输入会在进入宿主前报告作者错误。"""
    if not isinstance(value, str) or not value.strip():
        raise ValueError(message)
    return value


def continueEvent(event: Event) -> Action:
    """保持当前事件不变；适用于所有阶段且不会物化正文副本。"""
    return Action(event.eventId, ActionKind.CONTINUE)


def modifyPayload(event: Event, payload: Any) -> Action:
    """原子替换当前 payload；宿主会在发布线上数据前复验阶段与字节结构。"""
    return Action(event.eventId, ActionKind.MODIFY, [{"op": "replace", "path": "", "value": payload}])


def modifyBytes(event: Event, content: bytes | bytearray | memoryview) -> Action:
    """替换当前数据块并保留其他 payload 字段；用于 TCP、UDP、WebSocket 与正文块。"""
    if not isinstance(event.payload, dict):
        raise ValueError("二进制事件 payload 必须是对象")
    payload = dict(event.payload)
    payload["bytes"] = list(bytes(content))
    return modifyPayload(event, payload)


def hold(event: Event) -> Action:
    """暂存流式事件，等待后续字节；仅应在宿主声明支持 hold 的阶段返回。"""
    return Action(event.eventId, ActionKind.HOLD)


def drop(event: Event) -> Action:
    """丢弃当前数据块或录制事务；实际语义由当前阶段确定。"""
    return Action(event.eventId, ActionKind.DROP)


def reject(event: Event, reason: str = "pluginRejected") -> Action:
    """拒绝当前操作并附带稳定原因；宿主负责映射为对应协议的终止行为。"""
    return Action(event.eventId, ActionKind.REJECT, output={"reason": requireNonBlank(reason, "拒绝原因不能为空")})


def close(event: Event, reason: str = "pluginClosed") -> Action:
    """请求立即关闭当前连接；非连接阶段使用时将由宿主结构校验拒绝。"""
    return Action(event.eventId, ActionKind.CLOSE, output={"reason": requireNonBlank(reason, "关闭原因不能为空")})


def annotate(event: Event, *annotations: dict[str, Any]) -> Action:
    """附加解码树、标签或显示信息而不改变线上字节。"""
    return Action(event.eventId, ActionKind.ANNOTATE, annotations=list(annotations))


def redirect(event: Event, host: str, port: int) -> Action:
    """改写最终上游目标；主机为空或端口越界时直接报告作者输入错误。"""
    if isinstance(port, bool) or not isinstance(port, int) or port < 1 or port > 65535:
        raise ValueError("重定向目标必须包含有效主机和端口")
    return Action(event.eventId, ActionKind.REDIRECT, output={"host": requireNonBlank(host, "重定向主机不能为空"), "port": port})


def respond(event: Event, response: dict[str, Any]) -> Action:
    """生成完整合成响应；响应结构由对应 HTTP、DNS 或命令阶段定义。"""
    return Action(event.eventId, ActionKind.RESPOND, output=response)
