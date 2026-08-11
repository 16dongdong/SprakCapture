import { type SyntheticEvent, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  supportsIncrementalMedia,
  useIncrementalMediaSource,
} from "./useIncrementalMediaSource";

type PreviewKind = "audio" | "image" | "page" | "unsupported" | "video";

interface BodyMediaPreviewProps {
  availableBytes?: number;
  bodyBytes?: Uint8Array;
  bodyBlob?: Blob;
  bodyUrl?: string;
  contentEncoding: string;
  contentType: string;
  sourceUrl: string;
}

interface PreviewFormat {
  kind: PreviewKind;
  mimeType: string;
}

interface PreviewGeneration {
  format: PreviewFormat;
  source: Blob | Uint8Array | string;
}

interface ObjectUrlState {
  generation: PreviewGeneration;
  url: string;
}

const utf8PrefixLength = 512;
const maximumIsoBoxCount = 4_096;
const maximumIsoBoxDepth = 8;

interface IsoBoxHeader {
  boxType: string;
  payloadStart: number;
  boxEnd: number;
}

/**
 * 判断字节前缀是否匹配固定文件签名；该函数只读取签名长度范围，短正文直接返回 false。
 */
function startsWithBytes(
  bodyBytes: Uint8Array,
  signature: readonly number[],
): boolean {
  return signature.every((byte, index) => bodyBytes[index] === byte);
}

/**
 * 读取 ISO BMFF 盒头，支持 32 位、64 位 extended size 与 size=0。
 *
 * `regionEnd` 是父容器硬边界；盒尺寸越界、不完整或超过 JavaScript 安全整数时返回 null，
 * 调用方停止解析当前容器，绝不在 mdat 负载内裸搜索媒体标记。
 */
function readIsoBoxHeader(
  bodyBytes: Uint8Array,
  boxStart: number,
  regionEnd: number,
): IsoBoxHeader | null {
  if (regionEnd - boxStart < 8) {
    return null;
  }
  const view = new DataView(
    bodyBytes.buffer,
    bodyBytes.byteOffset,
    bodyBytes.byteLength,
  );
  const size32 = view.getUint32(boxStart);
  const boxType = new TextDecoder("ascii").decode(
    bodyBytes.subarray(boxStart + 4, boxStart + 8),
  );
  let boxSize = size32;
  let headerBytes = 8;
  if (size32 === 0) {
    boxSize = regionEnd - boxStart;
  } else if (size32 === 1) {
    if (regionEnd - boxStart < 16) {
      return null;
    }
    const extendedSize = view.getBigUint64(boxStart + 8);
    if (extendedSize > BigInt(Number.MAX_SAFE_INTEGER)) {
      return null;
    }
    boxSize = Number(extendedSize);
    headerBytes = 16;
  }
  const boxEnd = boxStart + boxSize;
  if (boxSize < headerBytes || boxEnd > regionEnd) {
    return null;
  }
  return { boxType, payloadStart: boxStart + headerBytes, boxEnd };
}

/**
 * 按 ISO BMFF 盒层级识别轨道类型，能够跳过大型 mdat 并定位文件尾部 moov。
 *
 * 只递归 moov/trak/mdia 必要容器，并且只接受 mdia 的直接 hdlr 子盒；最大深度和盒数量
 * 均有硬上限。损坏或不完整盒返回已有证据，不用扩展名或 MIME 猜测轨道。
 */
function inspectIsoBmff(bodyBytes: Uint8Array): {
  isIsoBmff: boolean;
  trackKind: "audio" | "video" | null;
} {
  const firstBox = readIsoBoxHeader(bodyBytes, 0, bodyBytes.length);
  const isIsoBmff =
    firstBox !== null &&
    ["ftyp", "styp", "moov"].includes(firstBox.boxType);
  if (!isIsoBmff) {
    return { isIsoBmff: false, trackKind: null };
  }
  let hasAudioTrack = false;
  let hasVideoTrack = false;
  const regions: Array<{
    cursor: number;
    regionEnd: number;
    depth: number;
    insideMedia: boolean;
  }> = [
    { cursor: 0, regionEnd: bodyBytes.length, depth: 0, insideMedia: false },
  ];
  let inspectedBoxes = 0;
  while (regions.length > 0 && inspectedBoxes < maximumIsoBoxCount) {
    const region = regions.pop();
    if (region === undefined) {
      break;
    }
    let { cursor } = region;
    while (cursor < region.regionEnd && inspectedBoxes < maximumIsoBoxCount) {
      const header = readIsoBoxHeader(bodyBytes, cursor, region.regionEnd);
      if (header === null) {
        break;
      }
      inspectedBoxes += 1;
      if (
        region.insideMedia &&
        header.boxType === "hdlr" &&
        header.boxEnd - header.payloadStart >= 12
      ) {
        const handlerType = new TextDecoder("ascii").decode(
          bodyBytes.subarray(header.payloadStart + 8, header.payloadStart + 12),
        );
        hasAudioTrack ||= handlerType === "soun";
        hasVideoTrack ||= handlerType === "vide";
      }
      const isContainer = ["moov", "trak", "mdia"].includes(header.boxType);
      if (
        isContainer &&
        region.depth < maximumIsoBoxDepth &&
        header.payloadStart < header.boxEnd
      ) {
        if (header.boxEnd < region.regionEnd) {
          regions.push({ ...region, cursor: header.boxEnd });
        }
        regions.push({
          cursor: header.payloadStart,
          regionEnd: header.boxEnd,
          depth: region.depth + 1,
          insideMedia: region.insideMedia || header.boxType === "mdia",
        });
        break;
      }
      cursor = header.boxEnd;
    }
  }
  return {
    isIsoBmff: true,
    trackKind: hasVideoTrack ? "video" : hasAudioTrack ? "audio" : null,
  };
}

/**
 * 从响应声明与文件签名解析可在线渲染格式；运行于浏览器正文查看阶段，声明缺失或错误时以字节签名纠正。
 * 参数只包含已录制正文和响应 Content-Type；无法可靠识别时返回 unsupported，禁止把任意二进制交给页面执行器。
 */
export function detectPreviewFormat(
  bodyBytes: Uint8Array,
  declaredContentType: string,
): PreviewFormat {
  const declaredMimeType = declaredContentType
    .split(";", 1)[0]
    .trim()
    .toLocaleLowerCase();
  // 文件签名比服务端声明更接近实际解码契约。先完成强签名识别，避免把
  // `ftyp + soun` 的 MP4 音频交给 MP3 解码器，这正是分段音频预览失败的根因。
  if (startsWithBytes(bodyBytes, [0x89, 0x50, 0x4e, 0x47])) {
    return { kind: "image", mimeType: "image/png" };
  }
  if (startsWithBytes(bodyBytes, [0xff, 0xd8, 0xff])) {
    return { kind: "image", mimeType: "image/jpeg" };
  }
  if (startsWithBytes(bodyBytes, [0x47, 0x49, 0x46, 0x38])) {
    return { kind: "image", mimeType: "image/gif" };
  }
  if (
    startsWithBytes(bodyBytes, [0x52, 0x49, 0x46, 0x46]) &&
    String.fromCharCode(...bodyBytes.slice(8, 12)) === "WEBP"
  ) {
    return { kind: "image", mimeType: "image/webp" };
  }
  if (startsWithBytes(bodyBytes, [0x49, 0x44, 0x33])) {
    return { kind: "audio", mimeType: "audio/mpeg" };
  }
  if (startsWithBytes(bodyBytes, [0x4f, 0x67, 0x67, 0x53])) {
    return { kind: "audio", mimeType: "audio/ogg" };
  }
  if (startsWithBytes(bodyBytes, [0x66, 0x4c, 0x61, 0x43])) {
    return { kind: "audio", mimeType: "audio/flac" };
  }
  if (
    startsWithBytes(bodyBytes, [0x52, 0x49, 0x46, 0x46]) &&
    String.fromCharCode(...bodyBytes.slice(8, 12)) === "WAVE"
  ) {
    return { kind: "audio", mimeType: "audio/wav" };
  }
  const isoBmff = inspectIsoBmff(bodyBytes);
  if (isoBmff.isIsoBmff) {
    const { trackKind } = isoBmff;
    if (trackKind === "audio") {
      return { kind: "audio", mimeType: "audio/mp4" };
    }
    if (trackKind === "video") {
      return { kind: "video", mimeType: "video/mp4" };
    }
    return { kind: "unsupported", mimeType: "application/mp4" };
  }
  if (
    bodyBytes.length >= 2 &&
    bodyBytes[0] === 0xff &&
    (bodyBytes[1] & 0xe6) === 0xe2
  ) {
    return { kind: "audio", mimeType: "audio/mpeg" };
  }
  if (
    bodyBytes.length >= 2 &&
    bodyBytes[0] === 0xff &&
    (bodyBytes[1] & 0xf6) === 0xf0
  ) {
    return { kind: "audio", mimeType: "audio/aac" };
  }
  const textPrefix = new TextDecoder()
    .decode(bodyBytes.slice(0, utf8PrefixLength))
    .trimStart();
  if (/^(?:<!doctype\s+html|<html\b)/i.test(textPrefix)) {
    return { kind: "page", mimeType: "text/html" };
  }
  if (/^<svg\b/i.test(textPrefix)) {
    return { kind: "image", mimeType: "image/svg+xml" };
  }
  if (declaredMimeType.startsWith("image/")) {
    return { kind: "image", mimeType: declaredMimeType };
  }
  if (declaredMimeType.startsWith("audio/")) {
    return { kind: "audio", mimeType: declaredMimeType };
  }
  if (declaredMimeType.startsWith("video/")) {
    return { kind: "video", mimeType: declaredMimeType };
  }
  if (
    declaredMimeType === "text/html" ||
    declaredMimeType === "application/xhtml+xml"
  ) {
    return { kind: "page", mimeType: declaredMimeType };
  }
  return { kind: "unsupported", mimeType: declaredMimeType };
}

/**
 * 解码 HTTP Content-Encoding 后再交给媒体组件；浏览器原生流式解压避免为大正文引入同步主线程解码器。
 * 仅接受 identity、gzip 与 deflate，未知或多重编码抛出错误并由预览区显示失败状态。
 */
async function decodeContentEncoding(
  bodyBytes: Uint8Array,
  contentEncoding: string,
): Promise<Uint8Array> {
  const normalizedEncoding = contentEncoding.trim().toLocaleLowerCase();
  if (normalizedEncoding === "" || normalizedEncoding === "identity") {
    return bodyBytes;
  }
  const decompressionFormat =
    normalizedEncoding === "x-gzip" ? "gzip" : normalizedEncoding;
  if (decompressionFormat !== "gzip" && decompressionFormat !== "deflate") {
    throw new Error("unsupportedContentEncoding");
  }
  const decodedStream = new Blob([bodyBytes])
    .stream()
    .pipeThrough(new DecompressionStream(decompressionFormat));
  return new Uint8Array(await new Response(decodedStream).arrayBuffer());
}

/**
 * 为捕获的 HTML 注入原始响应地址作为 base；页面仍运行在 sandbox 中，但相对图片、样式与链接可按原页面解析。
 */
function buildPageDocument(bodyBytes: Uint8Array, sourceUrl: string): string {
  const html = new TextDecoder().decode(bodyBytes);
  const escapedSourceUrl = sourceUrl
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
  const baseElement = `<base href="${escapedSourceUrl}">`;
  const headMatch = /<head\b[^>]*>/i.exec(html);
  if (headMatch === null) {
    return `${baseElement}${html}`;
  }
  const insertOffset = headMatch.index + headMatch[0].length;
  return `${html.slice(0, insertOffset)}${baseElement}${html.slice(insertOffset)}`;
}

/**
 * 使用浏览器原生媒体组件在线渲染已录制正文；对象 URL 只在当前正文生命周期存在，切换事务时立即回收。
 * 页面通过 sandbox iframe 隔离，正文类型无法识别或浏览器解码失败时显示明确状态，不回退执行未知内容。
 */
export function BodyMediaPreview({
  availableBytes,
  bodyBytes,
  bodyBlob,
  bodyUrl,
  contentEncoding,
  contentType,
  sourceUrl,
}: BodyMediaPreviewProps) {
  const { t } = useTranslation();
  const [decodedBodyBytes, setDecodedBodyBytes] = useState<Uint8Array | null>(
    null,
  );
  const [decodeFailed, setDecodeFailed] = useState(false);
  const format = useMemo(
    () =>
      bodyUrl !== undefined
        ? detectPreviewFormat(new Uint8Array(), contentType)
        : bodyBlob !== undefined
        ? detectPreviewFormat(new Uint8Array(), bodyBlob.type || contentType)
        : decodedBodyBytes === null
        ? null
        : detectPreviewFormat(decodedBodyBytes, contentType),
    [bodyBlob, bodyUrl, contentType, decodedBodyBytes],
  );
  const previewGeneration = useMemo<PreviewGeneration | null>(() => {
    const source = bodyUrl ?? bodyBlob ?? decodedBodyBytes;
    return source === null || format === null ? null : { format, source };
  }, [bodyBlob, bodyUrl, decodedBodyBytes, format]);
  const incrementalSourceUrl =
    bodyUrl !== undefined &&
    (format?.kind === "audio" || format?.kind === "video")
      ? bodyUrl
      : undefined;
  const incrementalMedia = useIncrementalMediaSource(
    incrementalSourceUrl,
    format?.mimeType ?? "application/octet-stream",
    availableBytes,
  );
  const [objectUrlState, setObjectUrlState] =
    useState<ObjectUrlState | null>(null);
  const [failedGeneration, setFailedGeneration] =
    useState<PreviewGeneration | null>(null);
  const [failedFallbackGeneration, setFailedFallbackGeneration] =
    useState<PreviewGeneration | null>(null);

  useEffect(() => {
    if (bodyBlob !== undefined || bodyUrl !== undefined) {
      setDecodedBodyBytes(null);
      setDecodeFailed(false);
      return undefined;
    }
    let disposed = false;
    setDecodedBodyBytes(null);
    setDecodeFailed(false);
    void decodeContentEncoding(bodyBytes ?? new Uint8Array(), contentEncoding)
      .then((decodedBytes) => {
        if (!disposed) {
          setDecodedBodyBytes(decodedBytes);
        }
      })
      .catch(() => {
        if (!disposed) {
          setDecodeFailed(true);
        }
      });
    return () => {
      disposed = true;
    };
  }, [bodyBlob, bodyBytes, bodyUrl, contentEncoding]);

  useEffect(() => {
    if (
      previewGeneration === null ||
      previewGeneration.format.kind === "unsupported" ||
      previewGeneration.format.kind === "page"
    ) {
      setObjectUrlState(null);
      return undefined;
    }
    if (typeof previewGeneration.source === "string") {
      setObjectUrlState({
        generation: previewGeneration,
        url: previewGeneration.source,
      });
      return undefined;
    }
    // generation 门控保证切换正文后的首个 render 就不再引用旧 URL；effect 清理负责回收资源。
    const nextObjectUrl = URL.createObjectURL(
      previewGeneration.source instanceof Blob
        ? previewGeneration.source
        : new Blob([previewGeneration.source], {
            type: previewGeneration.format.mimeType,
          }),
    );
    setObjectUrlState({
      generation: previewGeneration,
      url: nextObjectUrl,
    });
    return () => URL.revokeObjectURL(nextObjectUrl);
  }, [previewGeneration]);

  const useIncrementalSource =
    incrementalSourceUrl !== undefined &&
    format !== null &&
    supportsIncrementalMedia(format.mimeType);
  // 非分片 MP4 等格式虽然声明支持 MSE，却可能拒绝 SourceBuffer 追加；此时保留同一媒体元素，
  // 仅把 src 切回后端 Range 端点，避免把浏览器能力差异误报成录制正文损坏。
  const objectUrl = useIncrementalSource && !incrementalMedia.failed
    ? incrementalMedia.url
    : objectUrlState?.generation === previewGeneration
      ? objectUrlState.url
      : null;
  const renderFailed =
    failedFallbackGeneration === previewGeneration ||
    (failedGeneration === previewGeneration && !incrementalMedia.failed);

  useEffect(() => {
    if (!incrementalMedia.failed) {
      return;
    }
    // SourceBuffer 与媒体元素可能为同一次 MSE 失败各发一个 error；切回 Range 地址时先清除
    // 同代错误，随后由 error 事件携带的 currentSrc 精确判断失败的究竟是旧 MSE 还是回退端点。
    setFailedGeneration((current) =>
      current === previewGeneration ? null : current,
    );
  }, [incrementalMedia.failed, previewGeneration]);

  /** 将解码失败绑定到产生事件的资源代际；旧元素迟到事件不会污染已经切换的新正文。 */
  const markCurrentGenerationFailed = (
    event: SyntheticEvent<
      HTMLIFrameElement | HTMLImageElement | HTMLMediaElement
    >,
  ) => {
    const failedSource =
      "currentSrc" in event.currentTarget && event.currentTarget.currentSrc !== ""
        ? event.currentTarget.currentSrc
        : event.currentTarget.src;
    if (
      incrementalMedia.failed &&
      incrementalMedia.url !== null &&
      failedSource === incrementalMedia.url
    ) {
      return;
    }
    if (previewGeneration !== null) {
      if (incrementalMedia.failed) {
        setFailedFallbackGeneration(previewGeneration);
      } else {
        setFailedGeneration(previewGeneration);
      }
    }
  };

  if (decodeFailed) {
    return (
      <div className="bodyMediaState viewerNotice--error">
        {t("viewer.body.previewFailed")}
      </div>
    );
  }
  if (
    format === null ||
    (decodedBodyBytes === null && bodyBlob === undefined && bodyUrl === undefined)
  ) {
    return <div className="bodyMediaState">{t("viewer.body.loading")}</div>;
  }
  if (format.kind === "unsupported") {
    return (
      <div className="bodyMediaState">
        {t("viewer.body.previewUnsupported")}
      </div>
    );
  }
  if (renderFailed) {
    return (
      <div className="bodyMediaState viewerNotice--error">
        {t("viewer.body.previewFailed")}
      </div>
    );
  }
  if (format.kind !== "page" && objectUrl === null) {
    return <div className="bodyMediaState">{t("viewer.body.loading")}</div>;
  }
  if (format.kind === "image") {
    return (
      <div className="bodyMediaPreview">
        <img
          key={objectUrl}
          alt={t("viewer.body.imagePreview")}
          src={objectUrl ?? undefined}
          onError={markCurrentGenerationFailed}
        />
      </div>
    );
  }
  if (format.kind === "audio") {
    return (
      <div className="bodyMediaPreview">
        <audio
          aria-label={t("viewer.body.audioPreview")}
          controls
          src={objectUrl ?? undefined}
          onError={markCurrentGenerationFailed}
        />
      </div>
    );
  }
  if (format.kind === "video") {
    return (
      <div className="bodyMediaPreview">
        <video
          aria-label={t("viewer.body.videoPreview")}
          controls
          src={objectUrl ?? undefined}
          onError={markCurrentGenerationFailed}
        />
      </div>
    );
  }
  return (
    <iframe
      className="bodyPagePreview"
      sandbox="allow-forms allow-scripts"
      referrerPolicy="no-referrer"
      srcDoc={buildPageDocument(decodedBodyBytes ?? new Uint8Array(), sourceUrl)}
      title={t("viewer.body.pagePreview")}
      onError={markCurrentGenerationFailed}
    />
  );
}
