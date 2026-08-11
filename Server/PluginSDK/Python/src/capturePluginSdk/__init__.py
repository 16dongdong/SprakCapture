"""公开 Python 插件作者使用的稳定 API。"""

from .actions import annotate, close, continueEvent, drop, hold, modifyBytes, modifyPayload, redirect, reject, respond
from .manifest import ManifestBuilder
from .model import Action, ActionKind, Event, EventContext, Stage
from .plugin import Plugin, definePlugin
from .simulator import Simulator, createInvocation
from .stream import Frame, FrameCodec, IdentityCipher, LengthPrefixedCodec, PayloadCipher, StreamPipeline


def serve(plugin: Plugin, concurrentInvocations: bool = False) -> None:
    """延迟加载 Sidecar 循环；作者可显式允许多个 invoke 同时在途。"""
    from .runner import run
    run(plugin, concurrentInvocations)

__all__ = [
    "Action", "ActionKind", "Event", "EventContext", "Frame", "FrameCodec", "IdentityCipher",
    "LengthPrefixedCodec", "ManifestBuilder", "PayloadCipher", "Plugin", "Simulator", "Stage",
    "StreamPipeline", "annotate", "close", "continueEvent", "createInvocation", "definePlugin",
    "drop", "hold", "modifyBytes", "modifyPayload", "redirect", "reject", "respond", "serve",
]
