"""为真实子进程测试提供可观测乱序与一次停止生命周期。"""

import os
import time
from pathlib import Path

from capturePluginSdk import Stage, continueEvent, definePlugin, serve

plugin = definePlugin()


@plugin.on(Stage.UDP_DATAGRAM)
def delay(event):
    """按夹具毫秒数延迟返回，用确定性时差验证多个 invoke 同时在途。"""
    time.sleep(float(event.payload.get("delayMs", 0)) / 1000)
    return continueEvent(event)


@plugin.onStop
def recordStop() -> None:
    """把一次停止生命周期写入系统临时探针；测试结束会删除该文件。"""
    probePath = os.environ.get("PLUGIN_STOP_PROBE")
    if probePath:
        Path(probePath).write_text("stopped", encoding="utf-8")


if __name__ == "__main__":
    serve(plugin, concurrentInvocations=True)
