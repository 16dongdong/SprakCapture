/** 实现连接级双向增量分包、解密、修改与重封包管线。 */

export interface Frame { readonly payload: Uint8Array; readonly metadata: Readonly<Record<string, unknown>>; }
export interface DecodeResult { readonly frame?: Frame; readonly consumedBytes: number; }
export interface FrameCodec { decode(bufferedBytes: Uint8Array): DecodeResult; encode(frame: Frame): Uint8Array; }
export interface PayloadCipher { decrypt(payload: Uint8Array): Uint8Array; encrypt(payload: Uint8Array): Uint8Array; }

export class IdentityCipher implements PayloadCipher {
  /** 原样返回明文，供无加密协议复用统一管线。 */
  public decrypt(payload: Uint8Array): Uint8Array { return payload; }
  /** 原样返回线正文，不创建第二份缓冲。 */
  public encrypt(payload: Uint8Array): Uint8Array { return payload; }
}

export class LengthPrefixedCodec implements FrameCodec {
  /** 配置固定宽度长度字段；非法参数在插件启动时失败。 */
  public constructor(private readonly prefixBytes = 4, private readonly byteOrder: "big" | "little" = "big", private readonly includesPrefix = false) {
    if (prefixBytes < 1 || prefixBytes > 6 || !Number.isInteger(prefixBytes)) throw new Error("长度前缀必须是 1..6 字节整数");
  }

  /** 增量提取一帧；数据不足不消费任何字节。 */
  public decode(bufferedBytes: Uint8Array): DecodeResult {
    if (bufferedBytes.length < this.prefixBytes) return { consumedBytes: 0 };
    let declaredLength = 0;
    const indices = this.byteOrder === "big" ? [...Array(this.prefixBytes).keys()] : [...Array(this.prefixBytes).keys()].reverse();
    for (const index of indices) declaredLength = declaredLength * 256 + (bufferedBytes[index] ?? 0);
    const payloadLength = declaredLength - (this.includesPrefix ? this.prefixBytes : 0);
    const frameLength = this.prefixBytes + payloadLength;
    if (payloadLength < 0) throw new Error("帧长度小于前缀宽度");
    if (bufferedBytes.length < frameLength) return { consumedBytes: 0 };
    return { frame: { payload: bufferedBytes.slice(this.prefixBytes, frameLength), metadata: {} }, consumedBytes: frameLength };
  }

  /** 重算长度并输出完整线帧；超过 JavaScript 安全整数或前缀容量时失败。 */
  public encode(frame: Frame): Uint8Array {
    let declaredLength = frame.payload.length + (this.includesPrefix ? this.prefixBytes : 0);
    const prefix = new Uint8Array(this.prefixBytes);
    for (let index = 0; index < this.prefixBytes; index += 1) {
      const target = this.byteOrder === "big" ? this.prefixBytes - index - 1 : index;
      prefix[target] = declaredLength % 256;
      declaredLength = Math.floor(declaredLength / 256);
    }
    if (declaredLength !== 0) throw new Error("重封包后的正文超过长度字段容量");
    const output = new Uint8Array(prefix.length + frame.payload.length);
    output.set(prefix);
    output.set(frame.payload, prefix.length);
    return output;
  }
}

type CodecFactory = (connectionId: string, direction: string) => FrameCodec;
type CipherFactory = (connectionId: string, direction: string) => PayloadCipher;

export class StreamPipeline {
  private readonly buffers = new Map<string, Uint8Array>();
  private readonly codecs = new Map<string, FrameCodec>();
  private readonly ciphers = new Map<string, PayloadCipher>();

  /** 保存状态工厂；每个连接方向只创建一次编解码器和密码器。 */
  public constructor(private readonly codecFactory: CodecFactory, private readonly cipherFactory: CipherFactory = () => new IdentityCipher()) {}

  /** 追加块并处理全部完整帧；undefined 表示仍在等待半帧。 */
  public push(connectionId: string, direction: string, chunk: Uint8Array, transform: (frame: Frame) => Frame | Uint8Array | null): Uint8Array | undefined {
    const key = `${connectionId}\u0000${direction}`;
    const previous = this.buffers.get(key) ?? new Uint8Array();
    let buffer = new Uint8Array(previous.length + chunk.length);
    buffer.set(previous);
    buffer.set(chunk, previous.length);
    const codec = this.codecs.get(key) ?? this.codecFactory(connectionId, direction);
    const cipher = this.ciphers.get(key) ?? this.cipherFactory(connectionId, direction);
    this.codecs.set(key, codec);
    this.ciphers.set(key, cipher);
    const outputs: Uint8Array[] = [];
    let consumedFrame = false;
    while (buffer.length > 0) {
      const decision = codec.decode(buffer);
      if (!decision.frame) break;
      if (decision.consumedBytes < 1 || decision.consumedBytes > buffer.length) throw new Error("编解码器返回了无效消费长度");
      buffer = buffer.slice(decision.consumedBytes);
      consumedFrame = true;
      const clearFrame = { payload: cipher.decrypt(decision.frame.payload), metadata: decision.frame.metadata };
      const changed = transform(clearFrame);
      if (changed === null) continue;
      const changedFrame = changed instanceof Uint8Array ? { payload: changed, metadata: clearFrame.metadata } : changed;
      outputs.push(codec.encode({ payload: cipher.encrypt(changedFrame.payload), metadata: changedFrame.metadata }));
    }
    this.buffers.set(key, buffer);
    if (outputs.length === 0) return consumedFrame ? new Uint8Array() : undefined;
    const totalLength = outputs.reduce((total, output) => total + output.length, 0);
    const joined = new Uint8Array(totalLength);
    let offset = 0;
    for (const output of outputs) { joined.set(output, offset); offset += output.length; }
    return joined;
  }

  /** 清理连接状态；存在残帧时明确失败，避免静默漏字节。 */
  public close(connectionId: string): void {
    const prefix = `${connectionId}\u0000`;
    const keys = [...this.buffers.keys()].filter((key) => key.startsWith(prefix));
    if (keys.some((key) => (this.buffers.get(key)?.length ?? 0) > 0)) throw new Error("连接关闭时仍存在未完成帧");
    for (const key of keys) { this.buffers.delete(key); this.codecs.delete(key); this.ciphers.delete(key); }
  }
}
