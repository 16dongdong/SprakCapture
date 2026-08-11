"""实现不依赖代理服务的确定性本地宿主模拟器。"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .model import Action, PROCESS_JSON_SAFE_INTEGER_MAX
from .plugin import Plugin


class Simulator:
    """按夹具顺序调用插件，并保留可断言的标准动作对象。"""

    def __init__(self, plugin: Plugin) -> None:
        """绑定待测插件；模拟器不改变插件状态，连接状态由插件自行管理。"""
        self.plugin = plugin

    def invoke(self, invocation: dict[str, Any]) -> Action:
        """运行单个 RuntimeInvocation；协议字段错误或作者异常直接传播给测试。"""
        return self.plugin.invoke(invocation)

    def runFixture(self, fixturePath: str | Path) -> list[Action]:
        """运行 JSON 数组或 JSONL 夹具；解析失败指出文件位置且不返回部分结果。"""
        path = Path(fixturePath)
        content = path.read_text(encoding="utf-8")
        parsed = json.loads(content)
        invocations = parsed if isinstance(parsed, list) else [parsed]
        return [self.invoke(invocation) for invocation in invocations]


def createInvocation(eventId: str, stage: str, payload: Any, options: dict[str, Any] | None = None) -> dict[str, Any]:
    """创建最小完整测试调用；仅供 SDK 测试和插件夹具使用，不伪造生产宿主状态。"""
    resolvedOptions = options or {}
    connectionId = resolvedOptions.get("connectionId", "connection-1")
    direction = str(resolvedOptions.get("direction", "up"))
    return {
        "pluginId": "example.binary",
        "moduleId": "transformer",
        "moduleKind": "streamTransformer",
        "envelope": {
            "apiVersion": "2.0.0",
            "eventId": eventId,
            "stage": stage,
            "serviceGeneration": 1,
            "recordingGeneration": 1,
            "pluginInstanceId": "example.binary@1.0.0#1",
            "connectionId": connectionId,
            "transactionId": None,
            "deadlineUnixMs": PROCESS_JSON_SAFE_INTEGER_MAX,
            "context": {"direction": direction, "interceptionMode": "intercept"},
            "payload": payload,
        },
    }
