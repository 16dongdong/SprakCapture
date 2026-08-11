"""演示两字节大端长度协议的分包、修改和重封包。"""

from capturePluginSdk import Frame, LengthPrefixedCodec, StreamPipeline, definePlugin, serve

plugin = definePlugin()
pipeline = StreamPipeline(lambda _connectionId, _direction: LengthPrefixedCodec(prefixBytes=2))


def rewriteFrame(frame: Frame, _event: object) -> bytes:
    """把完整明文帧转换为大写；SDK 会自动重算长度并输出完整线帧。"""
    return frame.payload.upper()


plugin.tcp(pipeline, rewriteFrame)

if __name__ == "__main__":
    serve(plugin)
