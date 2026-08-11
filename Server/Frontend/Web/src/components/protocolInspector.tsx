import { Braces, CircleCheck, CircleX, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  DecodedProtobufView,
  MessageSide,
  ValidateConfiguration,
  ValidationReport,
  ValidatorId,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { showIndependentWindow } from "../platform/independentWindowContract";
import { subscribeIndependentWindowResults } from "../platform/independentWindowEvents";

type ReadState<T> =
  | { kind: "loading" }
  | { kind: "ready"; value: T }
  | { kind: "error" };

interface ProtocolInspectorProps {
  transactionId: string;
  responseContentType: string;
  side?: MessageSide;
  showResponseValidation?: boolean;
}

/**
 * 判断内容类型优先使用的离线校验器；在线校验不作为自动选择，防止普通查看动作触发外发确认。
 */
function preferredValidatorId(contentType: string): ValidatorId {
  return contentType.toLocaleLowerCase().includes("json")
    ? "jsonSchema"
    : "htmlWellFormed";
}

/**
 * 返回当前内容类型可直接执行的已启用校验器；缺失时由界面展示明确状态而不是猜测服务端配置。
 */
function enabledValidators(
  configuration: ValidateConfiguration,
  contentType: string,
): ValidatorId[] {
  const preferred = preferredValidatorId(contentType);
  return configuration.validators
    .filter((validator) => validator.enabled)
    .map((validator) => validator.id)
    .sort((left, right) => {
      if (left === preferred) {
        return -1;
      }
      if (right === preferred) {
        return 1;
      }
      return left.localeCompare(right);
    });
}

/**
 * 将稳定的校验问题键映射至当前语言；未知在线问题只显示通用说明，不回显第三方响应内容。
 */
function presentValidationIssue(
  messageKey: string,
  translate: (key: string, options?: Record<string, unknown>) => string,
): string {
  const translated = translate(`viewer.protocol.issues.${messageKey}`);
  return translated.startsWith("viewer.protocol.issues.")
    ? translate("viewer.protocol.issues.unknown")
    : translated;
}

/**
 * 将服务端稳定的 Protobuf 解码错误码映射为当前语言文案，禁止把原始 code 直接展示给用户。
 *
 * 运行上下文：decode API 在无路由/未启用等场景返回 decodeError 字符串；查看器只读展示。
 * 失败语义：未知错误码回退到带 code 的通用模板，不丢信息。
 */
function presentProtobufDecodeError(
  code: string,
  translate: (key: string, options?: Record<string, unknown>) => string,
): string {
  const localizedKey = `viewer.protocol.protobuf.errors.${code}`;
  const translated = translate(localizedKey);
  if (translated === localizedKey || translated.startsWith("viewer.protocol.protobuf.errors.")) {
    return translate("viewer.protocol.protobuf.decodeError", { code });
  }
  return translated;
}

/**
 * 渲染事务的协议辅助视图，按需读取 Protobuf 解码结果和响应校验能力。
 *
 * 运行上下文：HTTP 响应可显示校验器；原始 TCP/UDP 方向只显示同方向 Protobuf 解码，避免把字节流误当 HTTP 响应。
 * 参数：transactionId 定位事务，responseContentType 参与校验器排序，side 固定消息方向，showResponseValidation 控制响应校验边界。
 * 失败语义：各读取请求独立显示失败状态，不影响正文、Hex 和事务导航。
 */
export function ProtocolInspector({
  transactionId,
  responseContentType,
  side = "response",
  showResponseValidation = true,
}: ProtocolInspectorProps) {
  const { t } = useTranslation();
  const {
    decodeProtobuf,
    getValidateConfiguration,
    getValidationReports,
    validateResponse,
  } = useServiceStore();
  const requestSequence = useRef(0);
  const [protobufState, setProtobufState] = useState<ReadState<DecodedProtobufView>>({
    kind: "loading",
  });
  const [validateState, setValidateState] = useState<ReadState<ValidateConfiguration>>({
    kind: "loading",
  });
  const [reportState, setReportState] = useState<ReadState<ValidationReport[]>>({
    kind: "loading",
  });
  const [selectedValidator, setSelectedValidator] = useState<ValidatorId | null>(null);
  const [validationPending, setValidationPending] = useState(false);

  /**
   * 刷新当前消息方向可用的协议数据，并通过请求序号与中止信号隔离过期结果。
   *
   * 运行上下文：切换事务、方向或正文类型时自动执行，也可由刷新按钮触发。
   * 参数：无，依赖当前组件属性与服务状态。
   * 失败语义：单项请求失败只更新对应读取状态；卸载或切换时中止尚未完成的请求。
   */
  const refresh = useCallback(() => {
    const abortController = new AbortController();
    const sequence = requestSequence.current + 1;
    requestSequence.current = sequence;
    setProtobufState({ kind: "loading" });
    setValidateState({ kind: "loading" });
    setReportState({ kind: "loading" });
    void decodeProtobuf(transactionId, side, abortController.signal)
      .then((value) => {
        if (requestSequence.current === sequence) {
          setProtobufState({ kind: "ready", value });
        }
      })
      .catch(() => {
        if (requestSequence.current === sequence) {
          setProtobufState({ kind: "error" });
        }
      });
    // 校验端点只接受 HTTP 响应正文；请求方向和原始流方向仅保留同方向的 Protobuf 解码。
    if (side === "request" || !showResponseValidation) {
      return () => {
        requestSequence.current += 1;
        abortController.abort();
      };
    }
    void getValidateConfiguration(abortController.signal)
      .then((value) => {
        if (requestSequence.current === sequence) {
          setValidateState({ kind: "ready", value });
          const available = enabledValidators(value, responseContentType);
          setSelectedValidator((current) =>
            current !== null && available.includes(current)
              ? current
              : (available[0] ?? null),
          );
        }
      })
      .catch(() => {
        if (requestSequence.current === sequence) {
          setValidateState({ kind: "error" });
        }
      });
    void getValidationReports(transactionId, abortController.signal)
      .then((value) => {
        if (requestSequence.current === sequence) {
          setReportState({ kind: "ready", value });
        }
      })
      .catch(() => {
        if (requestSequence.current === sequence) {
          setReportState({ kind: "error" });
        }
      });
    return () => {
      requestSequence.current += 1;
      abortController.abort();
    };
  }, [
    decodeProtobuf,
    getValidateConfiguration,
    getValidationReports,
    responseContentType,
    showResponseValidation,
    side,
    transactionId,
  ]);

  useEffect(() => refresh(), [refresh]);

  /** 在线校验在独立窗口完成后刷新报告；其它事务的结果不会打断当前查看状态。 */
  useEffect(
    () =>
      subscribeIndependentWindowResults((result) => {
        if (
          result.kind === "onlineValidation" &&
          result.transactionId === transactionId
        ) {
          refresh();
        }
      }),
    [refresh, transactionId],
  );

  /**
   * 执行已选择的响应校验器。
   *
   * 运行上下文：仅处理当前页面的离线校验；在线校验由独立窗口确认并执行。
   * 参数：onlineUploadConfirmed 对离线调用固定为 false，保留与后端请求结构一致。
   * 失败语义：调用失败显示报告读取错误，完成后始终解除忙碌状态。
   */
  const runValidation = useCallback(
    (onlineUploadConfirmed: boolean) => {
      if (selectedValidator === null || validationPending) {
        return;
      }
      setValidationPending(true);
      void validateResponse(transactionId, {
        validatorId: selectedValidator,
        onlineUploadConfirmed,
      })
        .then((report) => {
          setReportState((current) => {
            const reports = current.kind === "ready" ? current.value : [];
            return {
              kind: "ready",
              value: [
                ...reports.filter(
                  (existing) => existing.validatorId !== report.validatorId,
                ),
                report,
              ],
            };
          });
        })
        .catch(() => setReportState({ kind: "error" }))
        .finally(() => setValidationPending(false));
    },
    [selectedValidator, transactionId, validateResponse, validationPending],
  );

  const requestValidation = () => {
    if (selectedValidator === "w3cHtmlOnline") {
      void showIndependentWindow({
        kind: "onlineValidation",
        transactionId,
        validatorId: selectedValidator,
      });
      return;
    }
    runValidation(false);
  };
  const availableValidators =
    validateState.kind === "ready" && validateState.value.enabled
      ? enabledValidators(validateState.value, responseContentType)
      : [];

  return (
    <div className="protocolInspector">
      <section className="protocolInspectorSection">
        <header>
          <Braces aria-hidden="true" size={16} />
          <h3>{t("viewer.protocol.protobuf.title")}</h3>
          <button type="button" onClick={refresh}>
            <RefreshCw aria-hidden="true" size={14} />
            {t("viewer.protocol.refresh")}
          </button>
        </header>
        {protobufState.kind === "loading" && (
          <p>{t("viewer.protocol.loading")}</p>
        )}
        {protobufState.kind === "error" && (
          <p className="viewerNotice viewerNotice--error">
            {t("viewer.protocol.loadFailed")}
          </p>
        )}
        {protobufState.kind === "ready" && protobufState.value.json !== null && (
          <pre className="protocolJsonView">
            {JSON.stringify(protobufState.value.json, null, 2)}
          </pre>
        )}
        {protobufState.kind === "ready" && protobufState.value.json === null && (
          <p className="viewerNotice">
            {protobufState.value.decodeError === null
              ? t("viewer.protocol.protobuf.noResult")
              : presentProtobufDecodeError(protobufState.value.decodeError, t)}
          </p>
        )}
      </section>
      {side === "response" && showResponseValidation && (
        <>
          <section className="protocolInspectorSection">
            <header>
              <CircleCheck aria-hidden="true" size={16} />
              <h3>{t("viewer.protocol.validate.title")}</h3>
            </header>
            {validateState.kind === "loading" && <p>{t("viewer.protocol.loading")}</p>}
            {validateState.kind === "error" && (
              <p className="viewerNotice viewerNotice--error">
                {t("viewer.protocol.loadFailed")}
              </p>
            )}
            {validateState.kind === "ready" && (
              <div className="protocolValidationActions">
                <label>
                  <span>{t("viewer.protocol.validate.validator")}</span>
                  <select
                    disabled={availableValidators.length === 0 || validationPending}
                    value={selectedValidator ?? ""}
                    onChange={(event) =>
                      setSelectedValidator(
                        event.target.value === "" ? null : (event.target.value as ValidatorId),
                      )
                    }
                  >
                    {availableValidators.length === 0 && <option value="">{t("viewer.protocol.validate.unavailable")}</option>}
                    {availableValidators.map((validator) => (
                      <option key={validator} value={validator}>
                        {t(`viewer.protocol.validate.validators.${validator}`)}
                      </option>
                    ))}
                  </select>
                </label>
                <button
                  className="primaryButton"
                  disabled={selectedValidator === null || validationPending}
                  type="button"
                  onClick={requestValidation}
                >
                  <CircleCheck aria-hidden="true" size={14} />
                  {validationPending
                    ? t("viewer.protocol.validate.running")
                    : t("viewer.protocol.validate.run")}
                </button>
              </div>
            )}
            {reportState.kind === "loading" && <p>{t("viewer.protocol.loading")}</p>}
            {reportState.kind === "error" && (
              <p className="viewerNotice viewerNotice--error">
                {t("viewer.protocol.validate.reportFailed")}
              </p>
            )}
            {reportState.kind === "ready" && reportState.value.length === 0 && (
              <p className="viewerNotice">{t("viewer.protocol.validate.noReports")}</p>
            )}
            {reportState.kind === "ready" && reportState.value.map((report) => (
              <article className="validationReport" key={report.validatorId}>
                <h4>{t(`viewer.protocol.validate.validators.${report.validatorId}`)}</h4>
                {report.issues.length === 0 ? (
                  <p className="validationSuccess">{t("viewer.protocol.validate.noIssues")}</p>
                ) : (
                  <ul>
                    {report.issues.map((issue, issueIndex) => (
                      <li key={`${report.validatorId}:${issueIndex}`}>
                        <CircleX aria-hidden="true" size={14} />
                        <span>{presentValidationIssue(issue.messageKey, t)}</span>
                        {issue.line !== null && (
                          <small>
                            {t("viewer.protocol.validate.position", {
                              line: issue.line,
                              column: issue.column ?? 1,
                            })}
                          </small>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </article>
            ))}
          </section>
        </>
      )}
    </div>
  );
}
