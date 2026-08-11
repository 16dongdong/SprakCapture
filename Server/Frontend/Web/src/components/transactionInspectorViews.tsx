import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  MessageSide,
  TransactionDetail,
  TransactionSummary,
} from "../api/protocol";
import {
  formatTransactionBytes,
  formatTransactionDuration,
  formatTransactionTimestamp,
  presentTransactionProtocol,
  presentTransactionStatusDetail,
  totalTransactionBytes,
} from "./transactionPresentation";

/**
 * 渲染名称和值组成的可展开属性组；标题按钮只控制本组字段，窄屏仍保留完整诊断信息而不截断值。
 */
function PropertyGroup({
  title,
  rows,
  defaultExpanded = true,
}: {
  title: string;
  rows: Array<[string, ReactNode]>;
  defaultExpanded?: boolean;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  return (
    <section className="transactionPropertyGroup">
      <header>
        <button
          aria-expanded={expanded}
          aria-label={`${expanded ? "收缩" : "展开"} ${title}`}
          className="propertyGroupToggle"
          onClick={() => setExpanded((current) => !current)}
          type="button"
        >
          {expanded ? "−" : "+"}
        </button>
        <h3>{title}</h3>
      </header>
      {expanded && (
        <dl>
          {rows.map(([name, value]) => (
            <div className="transactionPropertyRow" key={name}>
              <dt>{name}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      )}
    </section>
  );
}

/**
 * 汇总所有启用的事务标记；没有标记时返回协议统一空值，避免展示内部布尔字段噪声。
 */
function enabledFlags(
  transaction: TransactionSummary,
  emptyValue: string,
  translate: ReturnType<typeof useTranslation>["t"],
): string {
  const activeFlags = Object.entries(transaction.flags)
    .filter(([, enabled]) => enabled)
    .map(([flagName]) => translate(`transactions.flags.${flagName}`));
  return activeFlags.length === 0 ? emptyValue : activeFlags.join(", ");
}

/**
 * 渲染事务概览；所有统计均来自当前摘要，详情尚未加载时仍可立即查看核心信息。
 */
export function OverviewView({
  transaction,
  clientProcessIcon,
  clientProcessPath,
}: {
  transaction: TransactionSummary;
  clientProcessIcon: string | null;
  clientProcessPath: string | null;
}) {
  const { t } = useTranslation();
  const emptyValue = t("transactions.table.emptyValue");
  const clientProcess =
    transaction.clientProcessName === null
      ? emptyValue
      : `${transaction.clientProcessName}${
          transaction.clientProcessId === null
            ? ""
            : ` (${transaction.clientProcessId})`
        }`;
  const clientProcessView =
    transaction.clientProcessName === null ? (
      emptyValue
    ) : (
      <span
        className="transactionClientProcess"
        title={clientProcessPath ?? undefined}
      >
        {clientProcessIcon !== null && (
          <img alt="" aria-hidden="true" src={clientProcessIcon} />
        )}
        <span>{clientProcess}</span>
      </span>
    );
  const errorRows: Array<[string, ReactNode]> =
    transaction.error === null
      ? []
      : [
          [t("viewer.overview.fields.errorCode"), transaction.error.code],
          [
            t("viewer.overview.fields.errorMessage"),
            t(transaction.error.messageKey, transaction.error.params),
          ],
        ];

  return (
    <div className="transactionInspectorScroll">
      <PropertyGroup
        title={t("viewer.overview.groups.transaction")}
        rows={[
          [
            t("viewer.overview.fields.transactionId"),
            transaction.transactionId,
          ],
          [
            t("viewer.overview.fields.recordingSessionId"),
            transaction.recordingSessionId,
          ],
          [
            t("viewer.overview.fields.protocol"),
            presentTransactionProtocol(transaction, t),
          ],
          [t("viewer.overview.fields.method"), transaction.method],
          [t("viewer.overview.fields.url"), transaction.urlDisplay],
          [
            t("viewer.overview.fields.status"),
            presentTransactionStatusDetail(transaction, t),
          ],
          [
            t("viewer.overview.fields.statusCode"),
            transaction.statusCode ?? emptyValue,
          ],
          [
            t("viewer.overview.fields.tags"),
            transaction.tags.join(", ") || emptyValue,
          ],
          [
            t("viewer.overview.fields.appliedTools"),
            transaction.appliedTools.join(", ") || emptyValue,
          ],
        ]}
      />
      <PropertyGroup
        defaultExpanded={false}
        title={t("viewer.overview.groups.connection")}
        rows={[
          [
            t("viewer.overview.fields.clientAddress"),
            transaction.clientAddress || emptyValue,
          ],
          [t("viewer.overview.fields.clientProcess"), clientProcessView],
        ]}
      />
      <PropertyGroup
        defaultExpanded={false}
        title={t("viewer.overview.groups.timings")}
        rows={[
          [
            t("viewer.overview.fields.startedAt"),
            formatTransactionTimestamp(
              transaction.timings.startAtMilliseconds,
              emptyValue,
            ),
          ],
          [
            t("viewer.overview.fields.endedAt"),
            formatTransactionTimestamp(
              transaction.timings.endAtMilliseconds,
              emptyValue,
            ),
          ],
          [
            t("viewer.overview.fields.duration"),
            formatTransactionDuration(transaction),
          ],
        ]}
      />
      <PropertyGroup
        title={t("viewer.overview.groups.sizes")}
        rows={[
          [
            t("viewer.overview.fields.requestHeaders"),
            formatTransactionBytes(transaction.sizes.requestHeaderBytes),
          ],
          [
            t("viewer.overview.fields.requestBody"),
            formatTransactionBytes(transaction.sizes.requestBodyBytes),
          ],
          [
            t("viewer.overview.fields.responseHeaders"),
            formatTransactionBytes(transaction.sizes.responseHeaderBytes),
          ],
          [
            t("viewer.overview.fields.responseBody"),
            formatTransactionBytes(transaction.sizes.responseBodyBytes),
          ],
          [
            t("viewer.overview.fields.total"),
            formatTransactionBytes(totalTransactionBytes(transaction)),
          ],
        ]}
      />
      <PropertyGroup
        defaultExpanded={false}
        title={t("viewer.overview.groups.flags")}
        rows={[
          [
            t("transactions.table.flags"),
            enabledFlags(transaction, emptyValue, t),
          ],
        ]}
      />
      {errorRows.length > 0 && (
        <PropertyGroup
          defaultExpanded={false}
          title={t("viewer.overview.groups.error")}
          rows={errorRows}
        />
      )}
    </div>
  );
}

/**
 * 汇总当前事务的网络结果与字节分布，供列表外的快速判断使用。
 *
 * 运行上下文：只读取 TransactionSummary，不等待正文或协议解码。
 * 参数：transaction 为当前选中的事务摘要。
 * 失败语义：缺失状态码与标签统一展示协议空值，不推断未采集数据。
 */
export function SummaryView({
  transaction,
}: {
  transaction: TransactionSummary;
}) {
  const { t } = useTranslation();
  const emptyValue = t("transactions.table.emptyValue");
  return (
    <div className="transactionSummaryView">
      <PropertyGroup
        title={t("viewer.tabs.summary")}
        rows={[
          [
            t("transactions.table.status"),
            presentTransactionStatusDetail(transaction, t),
          ],
          [
            t("transactions.table.statusCode"),
            transaction.statusCode ?? emptyValue,
          ],
          [
            t("transactions.table.duration"),
            formatTransactionDuration(transaction),
          ],
          [
            t("transactions.table.size"),
            formatTransactionBytes(totalTransactionBytes(transaction)),
          ],
        ]}
      />
      <PropertyGroup
        title={t("viewer.tabs.request")}
        rows={[
          [
            t("viewer.overview.fields.requestHeaders"),
            formatTransactionBytes(transaction.sizes.requestHeaderBytes),
          ],
          [
            t("viewer.overview.fields.requestBody"),
            formatTransactionBytes(transaction.sizes.requestBodyBytes),
          ],
          [t("transactions.table.host"), transaction.host],
          [t("transactions.table.path"), transaction.path || "/"],
        ]}
      />
      <PropertyGroup
        title={t("viewer.tabs.response")}
        rows={[
          [
            t("viewer.overview.fields.responseHeaders"),
            formatTransactionBytes(transaction.sizes.responseHeaderBytes),
          ],
          [
            t("viewer.overview.fields.responseBody"),
            formatTransactionBytes(transaction.sizes.responseBodyBytes),
          ],
          [
            t("transactions.table.contentType"),
            transaction.contentType || emptyValue,
          ],
          [
            t("viewer.overview.fields.appliedTools"),
            enabledFlags(transaction, emptyValue, t),
          ],
        ]}
      />
    </div>
  );
}

interface TimingSegment {
  label: string;
  durationMilliseconds: number;
  offsetMilliseconds: number;
}

/**
 * 按已观察到的边界构造非重叠时序段。
 *
 * 运行上下文：不同协议可能只上报部分时间点；每一段以前一已知边界为起点，避免把缺失阶段伪造为零。
 * 参数：transaction 为当前事务摘要。
 * 失败语义：无有效结束时间或时间倒退时跳过对应阶段，最终返回空数组或可用的下载阶段。
 */
function timingSegments(transaction: TransactionSummary): TimingSegment[] {
  const startAt = transaction.timings.startAtMilliseconds;
  const endAt = transaction.timings.endAtMilliseconds;
  if (endAt === null || endAt <= startAt) {
    return [];
  }
  const boundaries: Array<[string, number | null]> = [
    ["DNS", transaction.timings.dnsEndAtMilliseconds],
    ["TCP", transaction.timings.connectEndAtMilliseconds],
    ["TLS", transaction.timings.tlsEndAtMilliseconds],
    ["HTTP", transaction.timings.requestSentAtMilliseconds],
    ["TTFB", transaction.timings.responseStartAtMilliseconds],
    ["BODY", endAt],
  ];
  let previousBoundary = startAt;
  const segments: TimingSegment[] = [];
  for (const [label, boundary] of boundaries) {
    if (boundary === null || boundary <= previousBoundary || boundary > endAt) {
      continue;
    }
    segments.push({
      label,
      durationMilliseconds: boundary - previousBoundary,
      offsetMilliseconds: previousBoundary - startAt,
    });
    previousBoundary = boundary;
  }
  return segments;
}

/**
 * 渲染单事务瀑布图；条宽只由真实观测时长计算。
 *
 * 运行上下文：列表与概览均使用同一 TransactionTimings，本视图不新增采集或后端计算。
 * 参数：transaction 为当前事务摘要。
 * 失败语义：阶段边界不完整时显示空值而不是给出错误的零毫秒瀑布。
 */
export function TimingChartView({
  transaction,
}: {
  transaction: TransactionSummary;
}) {
  const { t } = useTranslation();
  const segments = timingSegments(transaction);
  const totalDuration =
    transaction.timings.endAtMilliseconds === null
      ? 0
      : transaction.timings.endAtMilliseconds -
        transaction.timings.startAtMilliseconds;
  if (segments.length === 0 || totalDuration <= 0) {
    return (
      <div className="emptyState">
        <strong>{t("viewer.tabs.chart")}</strong>
        <span>{t("transactions.table.emptyValue")}</span>
      </div>
    );
  }
  return (
    <div className="timingChartView">
      <header>
        <strong>{t("viewer.tabs.chart")}</strong>
        <span>{formatTransactionDuration(transaction)}</span>
      </header>
      <div className="timingWaterfall" role="list">
        {segments.map((segment, index) => (
          <div
            className="timingWaterfallRow"
            key={`${segment.label}:${index}`}
            role="listitem"
          >
            <span>{segment.label}</span>
            <div className="timingWaterfallTrack">
              <i
                className={`timingWaterfallBar timingWaterfallBar--${index % 6}`}
                style={{
                  left: `${
                    (segment.offsetMilliseconds / totalDuration) * 100
                  }%`,
                  width: `${Math.max(
                    1,
                    (segment.durationMilliseconds / totalDuration) * 100,
                  )}%`,
                }}
              />
            </div>
            <output>{segment.durationMilliseconds} ms</output>
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * 渲染后端事务备注；M1d 只读展示录制值，不伪造本地备注或未定义的写回协议。
 */
export function NotesView({ notes }: { notes: string }) {
  const { t } = useTranslation();
  return (
    <div className="transactionNotesView">
      <h3>{t("viewer.notes.title")}</h3>
      <pre>{notes || t("viewer.notes.empty")}</pre>
      <span>{t("viewer.notes.readOnly")}</span>
    </div>
  );
}

/**
 * 汇总一个流方向的核心计数，供概览页快速判断录制完整性。
 *
 * 运行上下文：用户选中原始 TCP/UDP 的请求或响应二级节点时渲染，不混入另一方向数据。
 * 参数：detail 为权威事务详情，side 为当前流方向。
 * 失败语义：正文元数据缺失时已录制字节显示为零，不推测未提交片段。
 */
export function StreamDirectionOverview({
  detail,
  side,
}: {
  detail: TransactionDetail;
  side: MessageSide;
}) {
  const requestSide = side === "request";
  const bodyMeta = requestSide ? detail.requestBody : detail.responseBody;
  const packets = requestSide ? detail.requestPackets : detail.responsePackets;
  const originalBytes = requestSide
    ? detail.transaction.sizes.requestBodyBytes
    : detail.transaction.sizes.responseBodyBytes;
  return (
    <dl className="streamDirectionSummary">
      <div>
        <dt>方向</dt>
        <dd>{requestSide ? "请求" : "响应"}</dd>
      </div>
      <div>
        <dt>数据包</dt>
        <dd>{packets.length}</dd>
      </div>
      <div>
        <dt>原始字节</dt>
        <dd>{formatTransactionBytes(originalBytes)}</dd>
      </div>
      <div>
        <dt>已录制</dt>
        <dd>{formatTransactionBytes(bodyMeta?.storedBytes ?? 0)}</dd>
      </div>
    </dl>
  );
}

/**
 * 渲染一个流方向的逐包摘要，区别于只显示聚合计数的概览页。
 *
 * 运行上下文：原始 TCP/UDP 方向的摘要页用于核对包序、相对时间以及截断情况，不读取正文内容。
 * 参数：detail 为权威事务详情，side 为当前流方向。
 * 失败语义：没有片段索引时显示明确空状态；时间早于事务起点时按零毫秒展示，避免负时间污染界面。
 */
export function StreamDirectionSummary({
  detail,
  side,
}: {
  detail: TransactionDetail;
  side: MessageSide;
}) {
  const packets =
    side === "request" ? detail.requestPackets : detail.responsePackets;
  const startAt = detail.transaction.timings.startAtMilliseconds;
  if (packets.length === 0) {
    return <div className="emptyState">当前方向没有可录制的数据包</div>;
  }
  return (
    <div
      className="streamDirectionPacketSummary"
      role="table"
      aria-label="数据包摘要"
    >
      <div className="streamDirectionPacketSummaryHeader" role="row">
        <span role="columnheader">数据包</span>
        <span role="columnheader">相对时间</span>
        <span role="columnheader">原始大小</span>
        <span role="columnheader">录制结果</span>
      </div>
      {packets.map((packet) => (
        <div key={packet.sequence} role="row">
          <span role="cell">#{packet.sequence}</span>
          <span role="cell">
            +{Math.max(0, packet.capturedAtMilliseconds - startAt)} ms
          </span>
          <span role="cell">
            {formatTransactionBytes(packet.originalBytes)}
          </span>
          <span role="cell">
            {packet.truncated
              ? `${formatTransactionBytes(packet.storedBytes)} · 已截断`
              : formatTransactionBytes(packet.storedBytes)}
          </span>
        </div>
      ))}
    </div>
  );
}

/**
 * 将方向内片段时间映射为带刻度的紧凑时间轴。
 *
 * 运行上下文：原始 TCP/UDP 方向的图表页只使用后端观测时间和包大小，不推测网络延迟。
 * 参数：detail 为权威事务详情，side 为当前流方向。
 * 失败语义：没有片段索引时显示明确空状态；乱序时间会被限制在有效时间轴范围内。
 */
export function StreamDirectionChart({
  detail,
  side,
}: {
  detail: TransactionDetail;
  side: MessageSide;
}) {
  const packets =
    side === "request" ? detail.requestPackets : detail.responsePackets;
  const startAt = detail.transaction.timings.startAtMilliseconds;
  if (packets.length === 0) {
    return <div className="emptyState">当前方向没有可录制的数据包</div>;
  }
  const latestAt = packets.reduce(
    (maximum, packet) => Math.max(maximum, packet.capturedAtMilliseconds),
    startAt,
  );
  const range = Math.max(1, latestAt - startAt);
  return (
    <div className="streamDirectionChart">
      <header>
        <strong>{side === "request" ? "请求时间轴" : "响应时间轴"}</strong>
        <span>{packets.length} 个数据包</span>
      </header>
      <div className="streamDirectionChartScale" aria-hidden="true">
        <span>0 ms</span>
        <span>+{range} ms</span>
      </div>
      <div className="streamDirectionChartRows" role="list">
        {packets.map((packet) => {
          const offset = Math.max(
            0,
            Math.min(range, packet.capturedAtMilliseconds - startAt),
          );
          return (
            <div key={packet.sequence} role="listitem">
              <span>#{packet.sequence}</span>
              <div className="streamDirectionChartTrack">
                <i style={{ left: `${(offset / range) * 100}%` }} />
              </div>
              <output>
                {formatTransactionBytes(packet.originalBytes)} · +{offset} ms
              </output>
            </div>
          );
        })}
      </div>
    </div>
  );
}
