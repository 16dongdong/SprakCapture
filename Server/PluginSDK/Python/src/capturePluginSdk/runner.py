"""把标准输入上的 Sidecar JSONL 协议映射到普通 Python 插件函数。"""

from __future__ import annotations

import importlib
import json
import sys
import threading

from .model import PROCESS_JSON_SAFE_INTEGER_MAX
from .plugin import Plugin


def loadPlugin(specification: str) -> Plugin:
    """从 module[:attribute] 载入插件；属性可为 Plugin 或返回 Plugin 的无参函数。"""
    moduleName, separator, attributeName = specification.partition(":")
    module = importlib.import_module(moduleName)
    exported = getattr(module, attributeName or "plugin")
    plugin = exported() if callable(exported) and not isinstance(exported, Plugin) else exported
    if not isinstance(plugin, Plugin):
        raise TypeError("入口必须导出 Plugin 或返回 Plugin 的函数")
    return plugin


class JsonLineWriter:
    """串行化多个作者线程的协议输出，保证每个 JSON 帧独占完整一行。"""

    def __init__(self) -> None:
        """创建进程级输出锁；锁只覆盖一次编码和写入，不包围作者函数。"""
        self.lock = threading.Lock()

    def write(self, message: dict[str, object]) -> None:
        """原子写出并刷新单条 JSON；编码或管道失败会传播给当前调度任务。"""
        encoded = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
        with self.lock:
            print(encoded, flush=True)


def run(plugin: Plugin, concurrentInvocations: bool = False) -> None:
    """执行 JSONL；默认串行，作者显式开启后允许 requestId 结果乱序返回。"""
    initialized = False
    writer = JsonLineWriter()
    activeTasks: set[threading.Thread] = set()
    activeTasksLock = threading.Lock()

    def parseRequestId(message: dict[str, object]) -> int:
        """校验进程协议请求号；安全整数上限保证所有官方语言都能无损关联响应。"""
        requestId = message.get("requestId")
        if isinstance(requestId, bool) or not isinstance(requestId, int) or requestId < 0 or requestId > PROCESS_JSON_SAFE_INTEGER_MAX:
            raise ValueError("requestId 必须是非负 JSON 安全整数")
        return requestId

    def invoke(message: dict[str, object]) -> None:
        """执行一个 invoke 并回显 requestId；异常转换为同请求 error 帧。"""
        requestId = parseRequestId(message)
        try:
            action = plugin.invoke(message["invocation"])  # type: ignore[arg-type]
            writer.write({"type": "result", "requestId": requestId, "action": action.toDict()})
        except Exception as error:
            writer.write({"type": "error", "requestId": requestId, "message": f"{type(error).__name__}: {error}"})
        finally:
            with activeTasksLock:
                activeTasks.discard(threading.current_thread())

    def waitAndStop() -> None:
        """等待所有并发作者任务结束后调用一次插件停止生命周期。"""
        while True:
            with activeTasksLock:
                tasks = list(activeTasks)
            if not tasks:
                break
            for task in tasks:
                task.join()
        plugin.stop()

    for line in sys.stdin:
        if not line.strip():
            continue
        message: dict[str, object] = {}
        try:
            message = json.loads(line)
            messageType = message.get("type")
            if messageType == "initialize":
                if initialized or message.get("apiVersion") != 2:
                    raise ValueError("初始化顺序或 API 版本无效")
                plugin.manifest = dict(message.get("manifest", {}))
                plugin.configuration = dict(message.get("configuration", {}))
                initialized = True
                writer.write({"type": "ready", "apiVersion": 2})
                continue
            elif messageType == "invoke":
                if not initialized:
                    raise ValueError("插件尚未初始化")
                parseRequestId(message)
                if concurrentInvocations:
                    task = threading.Thread(target=invoke, args=(message,), name=f"plugin-{message.get('requestId')}")
                    with activeTasksLock:
                        activeTasks.add(task)
                    task.start()
                else:
                    invoke(message)
                continue
            elif messageType == "stop":
                waitAndStop()
                return
            else:
                raise ValueError("未知 Sidecar 消息")
        except Exception as error:
            requestId = message.get("requestId") if isinstance(message, dict) else None
            if isinstance(requestId, bool) or not isinstance(requestId, int) or requestId < 0 or requestId > PROCESS_JSON_SAFE_INTEGER_MAX:
                print(f"Sidecar 协议错误：{type(error).__name__}: {error}", file=sys.stderr)
                waitAndStop()
                return
            response = {"type": "error", "requestId": requestId, "message": f"{type(error).__name__}: {error}"}
        writer.write(response)
    waitAndStop()


def main() -> int:
    """启动独立开发运行器；生产插件也可在自己的入口脚本中直接调用 serve。"""
    arguments = [argument for argument in sys.argv[1:] if argument != "--concurrent"]
    if len(arguments) != 1:
        print("用法：python -m capturePluginSdk.runner module[:attribute] [--concurrent]", file=sys.stderr)
        return 2
    run(loadPlugin(arguments[0]), concurrentInvocations="--concurrent" in sys.argv[1:])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


def serve(plugin: Plugin, concurrentInvocations: bool = False) -> None:
    """启动 Sidecar 循环；并发模式由作者显式选择，Host 不施加线程数量限制。"""
    run(plugin, concurrentInvocations)
