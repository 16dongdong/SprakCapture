"""提供装饰器式插件注册与 TCP/UDP 高层处理器。"""

from __future__ import annotations

from collections.abc import Callable

from .actions import continueEvent, drop, hold, modifyBytes
from .model import Action, Event, Stage
from .stream import Frame, StreamPipeline

EventHandler = Callable[[Event], Action | None]
StopHandler = Callable[[], None]


class Plugin:
    """保存阶段回调与可选流管线；invoke 是模拟器和桥接运行器的唯一入口。"""

    def __init__(self) -> None:
        """创建空插件注册表；未注册阶段默认透明继续。"""
        self.handlers: dict[Stage, EventHandler] = {}
        self.manifest: dict[str, object] = {}
        self.configuration: dict[str, object] = {}
        self.streamPipeline: StreamPipeline | None = None
        self.frameHandler: Callable[[Frame, Event], Frame | bytes | None] | None = None
        self.udpHandler: Callable[[bytes, Event], bytes | None] | None = None
        self.stopHandler: StopHandler | None = None
        self.stopped = False

    def on(self, stage: Stage) -> Callable[[EventHandler], EventHandler]:
        """注册阶段函数；重复注册同一阶段立即失败，避免调用顺序隐藏。"""
        def register(handler: EventHandler) -> EventHandler:
            """把作者函数绑定到闭包指定阶段；重复绑定时不覆盖已有函数。"""
            if stage in self.handlers:
                raise ValueError(f"阶段已注册：{stage.value}")
            self.handlers[stage] = handler
            return handler
        return register

    def tcp(self, pipeline: StreamPipeline, handler: Callable[[Frame, Event], Frame | bytes | None]) -> None:
        """安装连接级 TCP 分包管线；每个完整明文帧像普通函数参数一样交给作者。"""
        self.streamPipeline = pipeline
        self.frameHandler = handler

    def udp(self, handler: Callable[[bytes, Event], bytes | None]) -> None:
        """安装逐数据报处理函数；返回 None 表示丢弃，返回 bytes 表示转发或修改。"""
        self.udpHandler = handler

    def onStop(self, handler: StopHandler) -> StopHandler:
        """注册一次性停止函数；重复注册会失败，避免资源释放顺序含糊。"""
        if self.stopHandler is not None:
            raise ValueError("停止函数已注册")
        self.stopHandler = handler
        return handler

    def stop(self) -> None:
        """在全部作者任务结束后执行一次生命周期清理；重复调用保持幂等。"""
        if self.stopped:
            return
        self.stopped = True
        if self.stopHandler is not None:
            self.stopHandler()

    def invoke(self, invocation: dict[str, object]) -> Action:
        """执行一次宿主调用；作者异常向桥接层传播并由宿主 failurePolicy 处理。"""
        event = Event.fromInvocation(invocation)
        if event.stage is Stage.TCP_CHUNK and self.streamPipeline and self.frameHandler:
            connectionId = event.connectionId or ""
            direction = event.context.direction or "unknown"
            output = self.streamPipeline.push(connectionId, direction, event.bytes, lambda frame: self.frameHandler(frame, event))
            if output is None:
                return hold(event)
            return modifyBytes(event, output) if output else drop(event)
        if event.stage is Stage.UDP_DATAGRAM and self.udpHandler:
            output = self.udpHandler(event.bytes, event)
            return modifyBytes(event, output) if output is not None else drop(event)
        handler = self.handlers.get(event.stage)
        return (handler(event) if handler else None) or continueEvent(event)


def definePlugin() -> Plugin:
    """创建插件定义对象；作者只需注册函数，不必实现运行时类或 JSON 分派。"""
    return Plugin()
