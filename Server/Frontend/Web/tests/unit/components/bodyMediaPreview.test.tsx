import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  BodyMediaPreview,
  detectPreviewFormat,
} from "@/components/bodyMediaPreview";

/**
 * 创建包含 mdat 伪标记与后置 moov/hdlr 的 ISO Base Media 测试正文。
 *
 * 运行上下文：测试无需构造可播放媒体，只验证预览器选择的解码契约。
 * 参数：`handlerType` 使用规范的 `soun` 或 `vide` 四字符轨道类型。
 * 失败语义：测试传入非四字符值时主动抛错，避免生成没有协议意义的夹具。
 */
function isoMediaFixture(handlerType: "soun" | "vide"): Uint8Array {
  const bytes = new Uint8Array(92);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, 16);
  bytes.set(new TextEncoder().encode("ftyp"), 4);
  bytes.set(new TextEncoder().encode("isom"), 8);
  view.setUint32(16, 1);
  bytes.set(new TextEncoder().encode("mdat"), 20);
  view.setBigUint64(24, 32n);
  // mdat 中的伪标记用于证明识别器不会裸搜索负载。
  bytes.set(new TextEncoder().encode("hdlr"), 32);
  bytes.set(new TextEncoder().encode(handlerType === "soun" ? "vide" : "soun"), 44);
  view.setUint32(48, 44);
  bytes.set(new TextEncoder().encode("moov"), 52);
  view.setUint32(56, 36);
  bytes.set(new TextEncoder().encode("trak"), 60);
  view.setUint32(64, 28);
  bytes.set(new TextEncoder().encode("mdia"), 68);
  view.setUint32(72, 20);
  bytes.set(new TextEncoder().encode("hdlr"), 76);
  bytes.set(new TextEncoder().encode(handlerType), 88);
  return bytes;
}

describe("正文媒体预览", () => {
  beforeEach(() => {
    let objectUrlSequence = 0;
    vi.stubGlobal("URL", {
      createObjectURL: vi.fn(() => `blob:body-preview-${++objectUrlSequence}`),
      revokeObjectURL: vi.fn(),
    });
  });

  it("在服务端类型缺失时通过 PNG 签名识别图片", () => {
    expect(
      detectPreviewFormat(
        new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]),
        "application/octet-stream",
      ),
    ).toEqual({ kind: "image", mimeType: "image/png" });
  });

  it("以 soun 轨道纠正被错误声明为 MP3 的 MP4 音频", () => {
    expect(
      detectPreviewFormat(isoMediaFixture("soun"), "audio/mpeg"),
    ).toEqual({ kind: "audio", mimeType: "audio/mp4" });
  });

  it("包含 vide 轨道时按 MP4 视频渲染", () => {
    expect(
      detectPreviewFormat(isoMediaFixture("vide"), "audio/mpeg"),
    ).toEqual({ kind: "video", mimeType: "video/mp4" });
  });

  it("使用纠正后的 audio/mp4 Blob 渲染音频控件", async () => {
    const { unmount } = render(
      <BodyMediaPreview
        bodyBytes={isoMediaFixture("soun")}
        contentEncoding="identity"
        contentType="audio/mpeg"
        sourceUrl="https://media.example/audio.mp4"
      />,
    );

    await waitFor(() =>
      expect(screen.getByLabelText("音频预览")).toHaveAttribute(
        "src",
        "blob:body-preview-1",
      ),
    );
    const blob = vi.mocked(URL.createObjectURL).mock.calls[0]?.[0];
    expect(blob).toBeInstanceOf(Blob);
    expect((blob as Blob).type).toBe("audio/mp4");
  });

  it("直接复用二进制响应 Blob 并在切换时回收对象 URL", async () => {
    const firstBlob = new Blob([new Uint8Array([1, 2, 3])], {
      type: "audio/mp4",
    });
    const secondBlob = new Blob([new Uint8Array([4, 5])], {
      type: "audio/mp4",
    });
    const { rerender, unmount } = render(
      <BodyMediaPreview
        bodyBlob={firstBlob}
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    const firstAudio = await screen.findByLabelText("音频预览");
    expect(URL.createObjectURL).toHaveBeenLastCalledWith(firstBlob);

    rerender(
      <BodyMediaPreview
        bodyBlob={secondBlob}
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    await waitFor(() =>
      expect(URL.createObjectURL).toHaveBeenLastCalledWith(secondBlob),
    );
    expect(screen.getByLabelText("音频预览")).toHaveAttribute(
      "src",
      "blob:body-preview-2",
    );
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:body-preview-1");
    // 旧媒体元素的迟到 error 只能标记旧 generation，不能覆盖当前新资源。
    fireEvent.error(firstAudio);
    expect(screen.getByLabelText("音频预览")).toHaveAttribute(
      "src",
      "blob:body-preview-2",
    );
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(2);
  });

  it("媒体端点 URL 直接交给浏览器流式解码且不创建 Blob URL", async () => {
    const { rerender } = render(
      <BodyMediaPreview
        bodyUrl="http://127.0.0.1:17890/media-preview"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    await waitFor(() =>
      expect(screen.getByLabelText("音频预览")).toHaveAttribute(
        "src",
        "http://127.0.0.1:17890/media-preview",
      ),
    );
    const audio = document.querySelector("audio");
    expect(audio).not.toBeNull();
    Object.defineProperty(audio!, "currentTime", { value: 12, writable: true });
    rerender(
      <BodyMediaPreview
        bodyUrl="http://127.0.0.1:17890/media-preview"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    expect(document.querySelector("audio")).toBe(audio);
    expect(audio!.currentTime).toBe(12);
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it("媒体增长时按 Range 追加字节且保持同一个播放元素", async () => {
    class TestSourceBuffer extends EventTarget {
      readonly chunks: Uint8Array[] = [];
      failNextAppend = false;

      /** 模拟浏览器异步提交片段；updateend 只在字节已进入缓冲区后触发。 */
      appendBuffer(bytes: BufferSource) {
        if (this.failNextAppend) {
          this.failNextAppend = false;
          queueMicrotask(() => this.dispatchEvent(new Event("error")));
          return;
        }
        this.chunks.push(new Uint8Array(bytes as ArrayBuffer));
        queueMicrotask(() => this.dispatchEvent(new Event("updateend")));
      }
    }
    const sources: TestMediaSource[] = [];
    class TestMediaSource extends EventTarget {
      readonly sourceBuffer = new TestSourceBuffer();

      constructor() {
        super();
        sources.push(this);
      }

      /** 测试夹具只声明 audio/mp4 可进行增量追加。 */
      static isTypeSupported(mimeType: string) {
        return mimeType === "audio/mp4";
      }

      /** 返回本代际唯一缓冲区，用于断言片段没有被拆到新播放器。 */
      addSourceBuffer() {
        return this.sourceBuffer;
      }
    }
    vi.stubGlobal("MediaSource", TestMediaSource);
    const fetchRange = vi.fn(async (_url: string, init?: RequestInit) => {
      const range = (init?.headers as Record<string, string> | undefined)?.Range;
      const start = range === "bytes=0-2" ? 0 : range === "bytes=3-4" ? 3 : 5;
      const bytes = start === 0 ? [1, 2, 3] : start === 3 ? [4, 5] : [6];
      return new Response(new Uint8Array(bytes), {
        status: 206,
        headers: { "content-range": `bytes ${start}-${start + bytes.length - 1}/5` },
      });
    });
    vi.stubGlobal("fetch", fetchRange);

    const { rerender, unmount } = render(
      <BodyMediaPreview
        availableBytes={3}
        bodyUrl="http://127.0.0.1:17890/media-preview"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    await waitFor(() => expect(sources).toHaveLength(1));
    const audio = document.querySelector("audio");
    act(() => sources[0]!.dispatchEvent(new Event("sourceopen")));
    await waitFor(() => expect(fetchRange).toHaveBeenCalledTimes(1));

    rerender(
      <BodyMediaPreview
        availableBytes={5}
        bodyUrl="http://127.0.0.1:17890/media-preview"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    await waitFor(() => expect(fetchRange).toHaveBeenCalledTimes(2));
    expect(fetchRange.mock.calls.map((call) => (call[1]?.headers as Record<string, string>).Range)).toEqual([
      "bytes=0-2",
      "bytes=3-4",
    ]);
    expect(document.querySelector("audio")).toBe(audio);
    expect(sources[0]!.sourceBuffer.chunks).toHaveLength(2);

    sources[0]!.sourceBuffer.failNextAppend = true;
    rerender(
      <BodyMediaPreview
        availableBytes={6}
        bodyUrl="http://127.0.0.1:17890/media-preview"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/ranged.mp4"
      />,
    );
    await waitFor(() =>
      expect(audio).toHaveAttribute(
        "src",
        "http://127.0.0.1:17890/media-preview",
      ),
    );
    Object.defineProperty(audio!, "currentSrc", {
      configurable: true,
      value: "blob:body-preview-1",
    });
    fireEvent.error(audio!);
    expect(document.querySelector("audio")).toBe(audio);
    unmount();
    vi.unstubAllGlobals();
  });

  it("MSE 没有迟到错误时回退 Range 的首次错误直接呈现失败", async () => {
    class FailingMediaSource extends EventTarget {
      /** 测试只让 audio/mp4 进入 MSE 分支。 */
      static isTypeSupported(mimeType: string) {
        return mimeType === "audio/mp4";
      }

      /** 模拟初始化缓冲区失败，使组件立即切回原生 Range 地址。 */
      addSourceBuffer(): SourceBuffer {
        throw new Error("测试缓冲区初始化失败");
      }
    }
    vi.stubGlobal("MediaSource", FailingMediaSource);
    const { unmount } = render(
      <BodyMediaPreview
        availableBytes={3}
        bodyUrl="http://127.0.0.1:17890/media-preview-fallback"
        contentEncoding="identity"
        contentType="audio/mp4"
        sourceUrl="https://media.example/fallback.mp4"
      />,
    );
    const mediaSource = await waitFor(() => {
      const source = vi.mocked(URL.createObjectURL).mock.calls[0]?.[0];
      expect(source).toBeInstanceOf(FailingMediaSource);
      return source as unknown as FailingMediaSource;
    });
    act(() => mediaSource.dispatchEvent(new Event("sourceopen")));
    const audio = await waitFor(() => {
      const element = document.querySelector("audio");
      expect(element).toHaveAttribute(
        "src",
        "http://127.0.0.1:17890/media-preview-fallback",
      );
      return element!;
    });
    Object.defineProperty(audio, "currentSrc", {
      configurable: true,
      value: "http://127.0.0.1:17890/media-preview-fallback",
    });

    fireEvent.error(audio);
    expect(document.querySelector("audio")).toBeNull();
    unmount();
    vi.unstubAllGlobals();
  });

  it("渲染音频控件并在卸载时回收对象 URL", async () => {
    const { unmount } = render(
      <BodyMediaPreview
        bodyBytes={new Uint8Array([0x49, 0x44, 0x33])}
        contentEncoding="identity"
        contentType="application/octet-stream"
        sourceUrl="https://media.example/audio.mp3"
      />,
    );
    await waitFor(() =>
      expect(screen.getByLabelText("音频预览")).toHaveAttribute(
        "src",
        "blob:body-preview-1",
      ),
    );
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:body-preview-1");
  });

  it("用沙箱 iframe 隔离页面正文", async () => {
    render(
      <BodyMediaPreview
        bodyBytes={new TextEncoder().encode(
          "<!doctype html><title>fixture</title>",
        )}
        contentEncoding="identity"
        contentType="text/html; charset=utf-8"
        sourceUrl="https://page.example/docs/index.html"
      />,
    );
    const frame = await screen.findByTitle("页面预览");
    expect(frame).toHaveAttribute("sandbox", "allow-forms allow-scripts");
    expect(frame).not.toHaveAttribute("allow-same-origin");
    expect(frame).toHaveAttribute(
      "srcdoc",
      expect.stringContaining(
        '<base href="https://page.example/docs/index.html">',
      ),
    );
  });
});
