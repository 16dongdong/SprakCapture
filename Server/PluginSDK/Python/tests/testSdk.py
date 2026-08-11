"""验证 Python SDK 的动作、流式协议和 Sidecar 线协议。"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

from capturePluginSdk import (
    Event, Frame, LengthPrefixedCodec, ManifestBuilder, Simulator, Stage, StreamPipeline,
    createInvocation, definePlugin, modifyBytes, modifyPayload, redirect, reject,
)


class SdkTests(unittest.TestCase):
    """覆盖作者最常用路径，确保 API 保持函数调用式且输出匹配 Host。"""

    def testStageHandlerBuildsModifyAction(self) -> None:
        """验证普通阶段函数可直接返回字节修改动作。"""
        plugin = definePlugin()

        @plugin.on(Stage.UDP_DATAGRAM)
        def rewrite(event):
            """反转数据报；错误由 SDK 字节校验直接传播。"""
            return modifyBytes(event, event.bytes[::-1])

        invocation = createInvocation("event-1", "udpDatagram", {"bytes": [1, 2, 3]})
        action = Simulator(plugin).invoke(invocation).toDict()
        self.assertEqual(action["action"], "modify")
        self.assertEqual(action["patch"][0]["value"]["bytes"], [3, 2, 1])

    def testTerminalActionsRejectWhitespace(self) -> None:
        """验证终止原因与目标主机的纯空白输入在进入宿主前被一致拒绝。"""
        invocation = createInvocation("event-validation", "beforeConnect", {"bytes": []})
        typedEvent = Event.fromInvocation(invocation)
        with self.assertRaises(ValueError):
            reject(typedEvent, " \t\r\n")
        with self.assertRaises(ValueError):
            redirect(typedEvent, " \t", 443)

    def testScalarPayloadRemainsLossless(self) -> None:
        """验证任意合法 JSON payload 原样进入作者函数，只有二进制便捷函数要求对象。"""
        typedEvent = Event.fromInvocation(createInvocation("event-scalar", "commandInvoked", 7))
        self.assertEqual(typedEvent.payload, 7)
        self.assertEqual(modifyPayload(typedEvent, ["ok"]).patch[0]["value"], ["ok"])
        with self.assertRaises(ValueError):
            modifyBytes(typedEvent, b"x")

    def testTcpPipelineKeepsHalfFrameAndRepackages(self) -> None:
        """验证半包返回 hold，后续块到达后一次发布重封装完整帧。"""
        plugin = definePlugin()
        pipeline = StreamPipeline(lambda _connectionId, _direction: LengthPrefixedCodec(2))
        plugin.tcp(pipeline, lambda frame, _event: Frame(frame.payload.upper(), frame.metadata))
        first = Simulator(plugin).invoke(createInvocation("event-1", "tcpChunk", {"bytes": [0, 3, 97]}))
        second = Simulator(plugin).invoke(createInvocation("event-2", "tcpChunk", {"bytes": [98, 99]}))
        self.assertEqual(first.action.value, "hold")
        self.assertEqual(second.patch[0]["value"]["bytes"], [0, 3, 65, 66, 67])

    def testManifestDeclaresSidecarEntry(self) -> None:
        """验证构造器输出宿主将接入的 sidecar/.py/JSONL 模型。"""
        manifest = ManifestBuilder("example.python", "示例", "1.0.0", "example").module("traffic", "trafficHandler", Stage.UDP_DATAGRAM).build("plugin.py")
        self.assertEqual(manifest["runtime"]["kind"], "sidecar")
        self.assertEqual(manifest["runtime"]["protocolVersion"], "2.0")

    def testRunnerImplementsCurrentJsonlProtocol(self) -> None:
        """以真实子进程验证 initialize、invoke、stop 的逐行刷新和请求关联。"""
        environment = dict(os.environ)
        sourceRoot = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "src"))
        exampleRoot = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "examples", "binaryProtocol"))
        environment["PYTHONPATH"] = os.pathsep.join([sourceRoot, exampleRoot])
        environment["PYTHONUTF8"] = "1"
        process = subprocess.Popen(
            [sys.executable, "-m", "capturePluginSdk.runner", "plugin:plugin"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            env=environment,
        )
        assert process.stdin and process.stdout
        initialize = {"type": "initialize", "apiVersion": 2, "manifest": {}, "configuration": {"mode": "test"}}
        invoke = {"type": "invoke", "requestId": 1, "invocation": createInvocation("event-1", "tcpChunk", {"bytes": [0, 1, 97]})}
        process.stdin.write(json.dumps(initialize) + "\n" + json.dumps(invoke) + "\n" + json.dumps({"type": "stop"}) + "\n")
        process.stdin.flush()
        ready = json.loads(process.stdout.readline())
        result = json.loads(process.stdout.readline())
        self.assertEqual(ready, {"type": "ready", "apiVersion": 2})
        self.assertEqual(result["type"], "result")
        self.assertEqual(result["requestId"], 1)
        self.assertIs(type(result["requestId"]), int)
        self.assertEqual(result["action"]["patch"][0]["value"]["bytes"], [0, 1, 65])
        self.assertEqual(process.wait(timeout=5), 0)
        process.stdin.close()
        process.stdout.close()
        assert process.stderr
        process.stderr.close()

    def testRunnerEnforcesProcessSafeIntegerBoundary(self) -> None:
        """真实进程接受安全整数上界并拒绝上界加一，防止跨语言 requestId 精度分叉。"""
        environment = dict(os.environ)
        sourceRoot = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "src"))
        exampleRoot = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "examples", "binaryProtocol"))
        environment["PYTHONPATH"] = os.pathsep.join([sourceRoot, exampleRoot])
        environment["PYTHONUTF8"] = "1"
        process = subprocess.Popen(
            [sys.executable, "-m", "capturePluginSdk.runner", "plugin:plugin"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            env=environment,
        )
        assert process.stdin and process.stdout and process.stderr
        maximumRequestId = 9_007_199_254_740_991
        messages = [
            {"type": "initialize", "apiVersion": 2, "manifest": {}, "configuration": {}},
            {"type": "invoke", "requestId": maximumRequestId, "invocation": createInvocation("event-max", "tcpChunk", {"bytes": [0, 1, 97]})},
            {"type": "invoke", "requestId": maximumRequestId + 1, "invocation": createInvocation("event-overflow", "tcpChunk", {"bytes": [0, 1, 98]})},
        ]
        process.stdin.write("".join(json.dumps(message) + "\n" for message in messages))
        process.stdin.flush()
        self.assertEqual(json.loads(process.stdout.readline()), {"type": "ready", "apiVersion": 2})
        result = json.loads(process.stdout.readline())
        self.assertEqual(result["requestId"], maximumRequestId)
        self.assertEqual(process.wait(timeout=5), 0)
        self.assertIn("requestId 必须是非负 JSON 安全整数", process.stderr.read())
        process.stdin.close()
        process.stdout.close()
        process.stderr.close()

    def testConcurrentRunnerReturnsOutOfOrderAndStopsOnce(self) -> None:
        """用真实进程验证显式并发、requestId 关联、原子输出及停止等待。"""
        environment = dict(os.environ)
        sourceRoot = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "src"))
        environment["PYTHONPATH"] = sourceRoot
        with tempfile.TemporaryDirectory(prefix="capture-python-sdk-") as temporaryDirectory:
            probePath = os.path.join(temporaryDirectory, "stopped.txt")
            environment["PLUGIN_STOP_PROBE"] = probePath
            entry = os.path.join(os.path.dirname(__file__), "concurrentPlugin.py")
            process = subprocess.Popen([sys.executable, entry], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", env=environment)
            assert process.stdin and process.stdout and process.stderr
            messages = [
                {"type": "initialize", "apiVersion": 2, "manifest": {}, "configuration": {}},
                {"type": "invoke", "requestId": 1, "invocation": createInvocation("slow-event", "udpDatagram", {"bytes": [1], "delayMs": 150})},
                {"type": "invoke", "requestId": 2, "invocation": createInvocation("fast-event", "udpDatagram", {"bytes": [2], "delayMs": 5})},
                {"type": "stop"},
            ]
            process.stdin.write("".join(json.dumps(message) + "\n" for message in messages))
            process.stdin.flush()
            responses = [json.loads(process.stdout.readline()) for _ in range(3)]
            self.assertEqual([response.get("requestId") for response in responses[1:]], [2, 1])
            self.assertTrue(all(type(response["requestId"]) is int for response in responses[1:]))
            self.assertEqual(process.wait(timeout=5), 0)
            self.assertEqual(Path(probePath).read_text(encoding="utf-8"), "stopped")
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()


if __name__ == "__main__":
    unittest.main()
