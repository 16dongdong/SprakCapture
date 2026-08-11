import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  type EditableHttpMessage,
  type HeaderField,
} from "../api/protocol";
import { IntegerField } from "./integerField";

/** 断点命中时可继续回写到代理流水线的消息草稿。 */
export type BreakpointPayload = EditableHttpMessage;

/** 区分当前编辑的是请求还是响应，避免在同一草稿中混入另一侧专属字段。 */
export type BreakpointMessagePhase = "request" | "response";

interface BreakpointMessageEditorProps {
  payload: BreakpointPayload;
  phase?: BreakpointMessagePhase;
  disabled?: boolean;
  onChange(next: BreakpointPayload): void;
}

interface HeaderEditorProps {
  headers: HeaderField[];
  disabled: boolean;
  onChange(headers: HeaderField[]): void;
}

const base64ChunkSize = 0x8000;

/**
 * 将 UTF-8 文本编码为标准 Base64；分块构造二进制字符串，避免大正文通过展开运算符时超出调用栈限制。
 */
function encodeUtf8Body(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += base64ChunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + base64ChunkSize));
  }
  return btoa(binary);
}

/**
 * 严格将标准 Base64 正文还原为 UTF-8；非规范 Base64 或二进制字节返回 null，保证原始正文不会被损坏。
 */
function decodeUtf8Body(value: string): string | null {
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    return null;
  }

  if (btoa(binary) !== value) {
    return null;
  }

  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }

  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** 根据协议字段推断编辑面向；调用方传入 phase 时始终以断点队列的权威阶段为准。 */
function resolveMessagePhase(
  payload: BreakpointPayload,
  phase: BreakpointMessagePhase | undefined,
): BreakpointMessagePhase {
  if (phase !== undefined) {
    return phase;
  }
  return payload.statusCode !== null || payload.reason !== null
    ? "response"
    : "request";
}

/** 将空输入标准化为 null，使未填写的可选协议字段不被错误地写成空字符串。 */
function nullableValue(value: string): string | null {
  return value === "" ? null : value;
}

/**
 * 渲染可重复 HTTP 头编辑表；头字段顺序与重复项保持不变，以匹配消息草稿的线性语义。
 */
function HeaderEditor({ headers, disabled, onChange }: HeaderEditorProps) {
  const { t } = useTranslation();

  /** 仅替换目标行，避免编辑一个头字段时改变其他重复头的相对顺序。 */
  const updateHeader = (
    index: number,
    field: keyof HeaderField,
    value: string,
  ) => {
    onChange(
      headers.map((header, headerIndex) =>
        headerIndex === index ? { ...header, [field]: value } : header,
      ),
    );
  };

  /** 在末尾追加空头字段，用户可按线性顺序构造重复的 HTTP 头。 */
  const addHeader = () => {
    onChange([...headers, { name: "", value: "" }]);
  };

  /** 删除指定头字段，不重排未删除项的相对顺序。 */
  const removeHeader = (index: number) => {
    onChange(headers.filter((_, headerIndex) => headerIndex !== index));
  };

  return (
    <section className="breakpointHeaderEditor">
      <div className="toolSectionHeading">
        <strong>{t("tools.form.headers")}</strong>
        <button disabled={disabled} type="button" onClick={addHeader}>
          <Plus aria-hidden="true" size={14} />
          {t("tools.form.addHeader")}
        </button>
      </div>
      {headers.length > 0 && (
        <div className="toolHeaderTableWrap">
          <table>
            <thead>
              <tr>
                <th scope="col">{t("tools.form.headerName")}</th>
                <th scope="col">{t("tools.form.headerValue")}</th>
                <th scope="col">
                  <span className="visuallyHidden">
                    {t("tools.form.removeHeader")}
                  </span>
                </th>
              </tr>
            </thead>
            <tbody>
              {headers.map((header, index) => (
                <tr key={index}>
                  <td>
                    <input
                      aria-label={`${t("tools.form.headerName")} ${index + 1}`}
                      disabled={disabled}
                      value={header.name}
                      onChange={(event) =>
                        updateHeader(index, "name", event.target.value)
                      }
                    />
                  </td>
                  <td>
                    <input
                      aria-label={`${t("tools.form.headerValue")} ${index + 1}`}
                      disabled={disabled}
                      value={header.value}
                      onChange={(event) =>
                        updateHeader(index, "value", event.target.value)
                      }
                    />
                  </td>
                  <td>
                    <button
                      aria-label={`${t("tools.form.removeHeader")} ${index + 1}`}
                      className="iconButton"
                      disabled={disabled}
                      type="button"
                      onClick={() => removeHeader(index)}
                    >
                      <Trash2 aria-hidden="true" size={14} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

/**
 * 以字段表单编辑断点消息；文本正文使用 UTF-8 文本框，二进制正文使用标准 Base64 输入以保持字节级可逆。
 */
export function BreakpointMessageEditor({
  payload,
  phase,
  disabled = false,
  onChange,
}: BreakpointMessageEditorProps) {
  const { t } = useTranslation();
  const messagePhase = resolveMessagePhase(payload, phase);
  const textBody = useMemo(
    () => decodeUtf8Body(payload.bodyBase64),
    [payload.bodyBase64],
  );
  /** 正文编辑模式按事务初始字节确定；清空二进制 Base64 后不能切换为文本编码，否则后续字节会被二次转码。 */
  const [bodyMode] = useState<"text" | "base64">(() =>
    textBody === null ? "base64" : "text",
  );

  /** 更新请求方法或 URL；空值恢复为协议的可选字段表示。 */
  const updateRequestField = (field: "method" | "url", value: string) => {
    onChange({ ...payload, [field]: nullableValue(value) });
  };

  /** 回写头字段集合，保留草稿中与当前阶段无关的字段不变。 */
  const updateHeaders = (headers: HeaderField[]) => {
    onChange({ ...payload, headers });
  };

  /** 将编辑后的 UTF-8 正文重新编码为标准 Base64，确保控制 API 接收稳定字节表示。 */
  const updateTextBody = (value: string) => {
    onChange({ ...payload, bodyBase64: encodeUtf8Body(value) });
  };

  return (
    <div className="breakpointMessageEditor">
      {messagePhase === "request" ? (
        <div className="toolFieldGrid breakpointMessageFields">
          <label>
            <span>{t("tools.form.method")}</span>
            <input
              aria-label={t("tools.form.method")}
              disabled={disabled}
              value={payload.method ?? ""}
              onChange={(event) =>
                updateRequestField("method", event.target.value)
              }
            />
          </label>
          <label className="toolWideField">
            <span>{t("tools.form.url")}</span>
            <input
              aria-label={t("tools.form.url")}
              disabled={disabled}
              type="url"
              value={payload.url ?? ""}
              onChange={(event) => updateRequestField("url", event.target.value)}
            />
          </label>
        </div>
      ) : (
        <div className="toolFieldGrid breakpointMessageFields">
          <IntegerField
            allowEmpty
            disabled={disabled}
            label={t("tools.form.statusCode")}
            max={599}
            min={100}
            value={payload.statusCode}
            onChange={(statusCode) => onChange({ ...payload, statusCode })}
            onEmpty={() => onChange({ ...payload, statusCode: null })}
          />
        </div>
      )}

      <HeaderEditor
        disabled={disabled}
        headers={payload.headers}
        onChange={updateHeaders}
      />

      {bodyMode === "base64" ? (
        <label className="toolTextAreaField breakpointBinaryBody">
          <span>{t("tools.form.binaryBody")}</span>
          <textarea
            aria-label={t("tools.form.binaryBody")}
            disabled={disabled}
            spellCheck={false}
            value={payload.bodyBase64}
            onChange={(event) =>
              onChange({ ...payload, bodyBase64: event.target.value })
            }
          />
          <small>{t("tools.form.binaryBodyHint")}</small>
        </label>
      ) : (
        <label className="toolTextAreaField breakpointBodyField">
          <span>{t("tools.form.body")}</span>
          <textarea
            aria-label={t("tools.form.body")}
            disabled={disabled}
            spellCheck={false}
            value={textBody ?? ""}
            onChange={(event) => updateTextBody(event.target.value)}
          />
        </label>
      )}
    </div>
  );
}
