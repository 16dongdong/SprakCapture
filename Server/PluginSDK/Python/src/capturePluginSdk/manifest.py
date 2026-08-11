"""提供不手写字段名的 manifest 构造器。"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .model import Stage


@dataclass(slots=True)
class ManifestBuilder:
    """构建 Host API 2.x Sidecar 清单；宿主按 .py 入口选择 Python。"""

    pluginId: str
    name: str
    version: str
    publisher: str
    description: str = field(default="Python 插件", init=False)
    modules: list[dict[str, Any]] = field(default_factory=list)

    def describe(self, description: str) -> "ManifestBuilder":
        """设置清单描述并返回构造器；空描述交给 Host 权威校验拒绝。"""
        self.description = description
        return self

    def module(self, moduleId: str, kind: str, *stages: Stage) -> "ManifestBuilder":
        """添加一个模块及订阅；空模块由宿主清单校验决定是否可用。"""
        self.modules.append({"id": moduleId, "kind": kind, "subscriptions": [{"stage": stage.value, "order": 0, "match": {}} for stage in stages], "contributes": []})
        return self

    def build(self, pythonEntry: str) -> dict[str, Any]:
        """生成可序列化清单；入口是可直接执行 JSONL 循环的相对 .py 文件。"""
        if not pythonEntry.endswith(".py"):
            raise ValueError("Python Sidecar 入口必须使用 .py 扩展名")
        return {
            "manifestVersion": 2,
            "id": self.pluginId,
            "name": self.name,
            "description": self.description,
            "version": self.version,
            "publisher": self.publisher,
            "engines": {"host": ">=2.0.0 <3.0.0", "api": "2.x"},
            "runtime": {"kind": "sidecar", "entry": pythonEntry, "protocolVersion": "2.0", "arguments": []},
            "modules": list(self.modules),
            "capabilities": [],
            "dependencies": {},
            "limits": {"timeoutMs": 0, "maxPendingEvents": 0, "maxOutputBytes": 0, "maxStorageBytes": 0},
            "failurePolicy": "failClosed",
            "extensions": {"pythonSdk": {"protocol": "jsonl"}},
        }
