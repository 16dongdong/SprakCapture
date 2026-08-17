/**
 * 汇总稳定协议导出；业务调用方继续只依赖 `@/api/protocol`，职责拆分不改变公共入口。
 * `protocolShared` 中仅数值边界属于公共契约，内部整数模式不得从这里暴露。
 */
export {
  maximumCachedCertificates,
  maximumConnections,
  maximumDescriptorEncodedCharacters,
  maximumEncodedBodyCharacters,
  maximumHttpCaptureBodyBytes,
  maximumHttpHeaderBytes,
  maximumHttpTimeoutMilliseconds,
  maximumPluginPackageBytes,
  maximumRecordingBodyBytes,
  maximumRecordingTotalBodyBytes,
  maximumRecordingTransactions,
  maximumRelayBufferSize,
  maximumShutdownTimeoutSeconds,
  maximumTotalRelayBufferSize,
  maximumTransactionCollectionTokenCharacters,
  maximumUdpPacketSize,
} from "./protocolShared";
export * from "./protocolCore";
export * from "./protocolTools";
export * from "./protocolRuntime";
