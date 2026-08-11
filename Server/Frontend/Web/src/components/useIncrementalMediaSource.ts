import { useEffect, useState } from "react";

interface IncrementalMediaState {
  failed: boolean;
  url: string | null;
}

interface MediaAppendSession {
  abortController: AbortController;
  disposed: boolean;
  loadedBytes: number;
  mediaSource: MediaSource;
  pumping: boolean;
  sourceBuffer: SourceBuffer | null;
  sourceUrl: string;
  targetBytes: number;
}

/** 等待 SourceBuffer 完成一次追加；解码器拒绝字节时以异常结束，不会继续拼接损坏数据。 */
function appendBytes(sourceBuffer: SourceBuffer, bytes: Uint8Array): Promise<void> {
  return new Promise((resolve, reject) => {
    /** 追加完成后解除成对监听，保证长媒体不会积累回调。 */
    const complete = () => {
      sourceBuffer.removeEventListener("error", fail);
      resolve();
    };
    /** 追加失败后解除成对监听，由预览层切回同一 Range 端点的原生播放器。 */
    const fail = () => {
      sourceBuffer.removeEventListener("updateend", complete);
      reject(new Error("浏览器拒绝追加媒体分段。"));
    };
    sourceBuffer.addEventListener("updateend", complete, { once: true });
    sourceBuffer.addEventListener("error", fail, { once: true });
    try {
      sourceBuffer.appendBuffer(bytes);
    } catch (error: unknown) {
      sourceBuffer.removeEventListener("updateend", complete);
      sourceBuffer.removeEventListener("error", fail);
      reject(error);
    }
  });
}

/**
 * 依次读取媒体端点新增长度并追加到同一个 SourceBuffer。
 * 端点必须以 206 返回精确 Range；任何错位或空响应都会终止当前代际，避免把坏包交给解码器。
 */
async function pumpAvailableMedia(session: MediaAppendSession): Promise<void> {
  if (
    session.disposed ||
    session.pumping ||
    session.sourceBuffer === null
  ) {
    return;
  }
  session.pumping = true;
  try {
    while (!session.disposed && session.loadedBytes < session.targetBytes) {
      const start = session.loadedBytes;
      const end = session.targetBytes - 1;
      const response = await fetch(session.sourceUrl, {
        cache: "no-store",
        headers: { Range: `bytes=${start}-${end}` },
        signal: session.abortController.signal,
      });
      const contentRange = response.headers.get("content-range");
      if (!response.ok || response.status !== 206 || !contentRange?.startsWith(`bytes ${start}-`)) {
        throw new Error("媒体增量端点未返回匹配的字节范围。");
      }
      const bytes = new Uint8Array(await response.arrayBuffer());
      if (bytes.byteLength === 0 || start + bytes.byteLength > session.targetBytes) {
        throw new Error("媒体增量端点返回了无效长度。 ");
      }
      await appendBytes(session.sourceBuffer, bytes);
      session.loadedBytes += bytes.byteLength;
    }
  } finally {
    session.pumping = false;
  }
}

/** 判断当前浏览器是否能用 MSE 增量追加该媒体类型；不支持时交回原生 Range 播放。 */
export function supportsIncrementalMedia(mimeType: string): boolean {
  return (
    typeof MediaSource !== "undefined" &&
    typeof MediaSource.isTypeSupported === "function" &&
    MediaSource.isTypeSupported(mimeType)
  );
}

/**
 * 在媒体 URL 不变的前提下按 capturedBytes 追加新片段。
 * sourceUrl 或 MIME 改变时才创建新媒体代际；普通事务刷新不会替换元素或归零播放位置。
 */
export function useIncrementalMediaSource(
  sourceUrl: string | undefined,
  mimeType: string,
  availableBytes: number | undefined,
): IncrementalMediaState {
  const supported = sourceUrl !== undefined && supportsIncrementalMedia(mimeType);
  const [state, setState] = useState<IncrementalMediaState>({
    failed: false,
    url: null,
  });
  const [session, setSession] = useState<MediaAppendSession | null>(null);

  useEffect(() => {
    if (!supported || sourceUrl === undefined) {
      setSession(null);
      setState({ failed: false, url: null });
      return undefined;
    }
    const mediaSource = new MediaSource();
    const objectUrl = URL.createObjectURL(mediaSource);
    const nextSession: MediaAppendSession = {
      abortController: new AbortController(),
      disposed: false,
      loadedBytes: 0,
      mediaSource,
      pumping: false,
      sourceBuffer: null,
      sourceUrl,
      targetBytes: Math.max(0, availableBytes ?? 0),
    };
    /** 初始化唯一 SourceBuffer；后续事件只推进目标长度，不创建新的媒体元素。 */
    const open = () => {
      try {
        nextSession.sourceBuffer = mediaSource.addSourceBuffer(mimeType);
        void pumpAvailableMedia(nextSession).catch(() => {
          if (!nextSession.disposed) {
            setState({ failed: true, url: objectUrl });
          }
        });
      } catch {
        setState({ failed: true, url: objectUrl });
      }
    };
    mediaSource.addEventListener("sourceopen", open, { once: true });
    setSession(nextSession);
    setState({ failed: false, url: objectUrl });
    return () => {
      nextSession.disposed = true;
      nextSession.abortController.abort();
      mediaSource.removeEventListener("sourceopen", open);
      URL.revokeObjectURL(objectUrl);
    };
  }, [mimeType, sourceUrl, supported]);

  useEffect(() => {
    if (session === null) {
      return;
    }
    session.targetBytes = Math.max(session.targetBytes, availableBytes ?? 0);
    void pumpAvailableMedia(session).catch(() => {
      if (!session.disposed) {
        setState((current) => ({ ...current, failed: true }));
      }
    });
  }, [availableBytes, session]);

  return state;
}
