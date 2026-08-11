import { CheckCircle2, FileKey2, FileUp, KeyRound, Trash2, Upload } from "lucide-react";
import {
  type ChangeEvent,
  type FormEvent,
  type RefObject,
  useEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import type {
  ClientCertificateFormat,
  LocationPattern,
  SslPublicState,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";

interface ClientCertificateManagerProps {
  state: SslPublicState;
  disabled: boolean;
  focusOnMount?: boolean;
  initialLocation?: LocationPattern | null;
}

interface CertificateFilePickerProps {
  accept: string;
  disabled: boolean;
  inputRef: RefObject<HTMLInputElement>;
  buttonRef?: RefObject<HTMLButtonElement>;
  label: string;
  required: boolean;
  selectedFile: File | null;
  onChange(event: ChangeEvent<HTMLInputElement>): void;
}

/**
 * 以显式按钮同步调用浏览器文件选择器，绕开原生复合 file 控件在独立 Chrome/WebView 窗口中的命中异常。
 * 运行上下文：showPicker 必须直接发生在用户点击回调内，浏览器才会授予文件选择器所需的瞬时激活权限。
 * 失败语义：输入未挂载代表组件结构损坏，直接抛错，不伪装成已经打开选择器。
 */
function CertificateFilePicker({
  accept,
  disabled,
  inputRef,
  buttonRef,
  label,
  required,
  selectedFile,
  onChange,
}: CertificateFilePickerProps) {
  const { t } = useTranslation();

  /** 在当前用户手势内直接请求系统文件选择器；失败表示目标浏览器缺少项目要求的文件选择能力。 */
  const openChooser = () => {
    const input = inputRef.current;
    if (input === null) {
      throw new Error("客户端证书文件输入未挂载");
    }
    input.showPicker();
  };

  const buttonText = selectedFile?.name ?? t("tools.form.chooseFile");
  return (
    <div className="sslClientCertificateFile">
      <span>{label}</span>
      <button
        aria-label={`${label}：${buttonText}`}
        className={`fileChoiceButton${selectedFile === null ? "" : " isSelected"}`}
        disabled={disabled}
        ref={buttonRef}
        type="button"
        onClick={openChooser}
      >
        {selectedFile === null ? (
          <FileUp aria-hidden="true" size={14} />
        ) : (
          <CheckCircle2 aria-hidden="true" size={14} />
        )}
        <span>{buttonText}</span>
      </button>
      <input
        accept={accept}
        aria-hidden="true"
        className="sslClientCertificateNativeFile"
        disabled={disabled}
        ref={inputRef}
        required={required}
        tabIndex={-1}
        type="file"
        onChange={onChange}
      />
    </div>
  );
}

/**
 * 管理按主机选择的上游 mTLS 客户端身份。
 * 运行上下文：该组件只持有浏览器 File 引用，提交后立即清空口令和文件选择。
 * 参数：state 是权威 SSL 快照，disabled 表示控制写槽被占用；initialLocation 预填右键主机和端口；
 * focusOnMount 让独立窗口直接定位到证书文件控件；失败由 Store 的统一错误区展示。
 */
export function ClientCertificateManager({
  state,
  disabled,
  focusOnMount = false,
  initialLocation = null,
}: ClientCertificateManagerProps) {
  const { t } = useTranslation();
  const { importClientCertificate, updateClientCertificate, removeClientCertificate } = useServiceStore();
  const initialHost = initialLocation?.host.trim() ?? "";
  const [name, setName] = useState(initialHost);
  const [host, setHost] = useState(initialHost);
  const [port, setPort] = useState(initialLocation?.port ?? "");
  const [format, setFormat] = useState<ClientCertificateFormat>("pkcs12");
  const [certificate, setCertificate] = useState<File | null>(null);
  const [privateKey, setPrivateKey] = useState<File | null>(null);
  const [password, setPassword] = useState("");
  const certificateInputRef = useRef<HTMLInputElement>(null);
  const certificateButtonRef = useRef<HTMLButtonElement>(null);
  const privateKeyInputRef = useRef<HTMLInputElement>(null);
  const sectionRef = useRef<HTMLElement>(null);

  /** 右键“添加客户端证书”打开独立窗口后定位到目标卡片；只移动可见焦点，不自动触发文件选择器。 */
  useEffect(() => {
    if (!focusOnMount) {
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      if (typeof sectionRef.current?.scrollIntoView === "function") {
        sectionRef.current.scrollIntoView({ block: "center" });
      }
      certificateButtonRef.current?.focus({ preventScroll: true });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [focusOnMount]);

  /** 提交完整身份文件和 HTTPS 主机规则；成功后清空所有敏感表单状态。 */
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const parsedPort = port === "" ? null : Number(port);
    if (
      certificate === null
      || host.trim() === ""
      || name.trim() === ""
      || (parsedPort !== null && (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65_535))
    ) {
      return;
    }
    const succeeded = await importClientCertificate({
      name,
      format,
      enabled: true,
      locations: [{ protocol: "https", host: host.trim(), port, path: "", query: null }],
      certificate,
      ...(privateKey === null ? {} : { privateKey }),
      password,
    });
    if (!succeeded) {
      return;
    }
    setName("");
    setHost("");
    setPort("");
    setCertificate(null);
    setPrivateKey(null);
    setPassword("");
    if (certificateInputRef.current !== null) certificateInputRef.current.value = "";
    if (privateKeyInputRef.current !== null) privateKeyInputRef.current.value = "";
  };

  return (
    <section
      className={`sslClientCertificateCard${focusOnMount ? " isTargeted" : ""}`}
      ref={sectionRef}
    >
      <header>
        <div>
          <strong>{t("ssl.clientCertificates.title")}</strong>
          <span>{t("ssl.clientCertificates.hint")}</span>
        </div>
        <KeyRound aria-hidden="true" size={18} />
      </header>

      <form className="sslClientCertificateForm" onSubmit={(event) => void submit(event)}>
        <label>
          <span>{t("ssl.clientCertificates.name")}</span>
          <input disabled={disabled} maxLength={80} required value={name} onChange={(event) => setName(event.target.value)} />
        </label>
        <label>
          <span>{t("ssl.clientCertificates.host")}</span>
          <input disabled={disabled} placeholder="api.example.com" required value={host} onChange={(event) => setHost(event.target.value)} />
        </label>
        <label>
          <span>{t("ssl.clientCertificates.port")}</span>
          <input disabled={disabled} inputMode="numeric" max={65_535} min={1} placeholder="443" type="number" value={port} onChange={(event) => setPort(event.target.value)} />
        </label>
        <label>
          <span>{t("ssl.clientCertificates.format")}</span>
          <select disabled={disabled} value={format} onChange={(event) => setFormat(event.target.value as ClientCertificateFormat)}>
            <option value="pkcs12">PKCS#12 / PFX</option>
            <option value="pem">PEM</option>
            <option value="der">DER</option>
          </select>
        </label>
        <CertificateFilePicker
          accept={format === "pkcs12" ? ".p12,.pfx" : format === "pem" ? ".pem,.crt,.cer" : ".der,.cer"}
          buttonRef={certificateButtonRef}
          disabled={disabled}
          inputRef={certificateInputRef}
          label={format === "pkcs12" ? "P12 / PFX" : t("ssl.clientCertificates.certificate")}
          required
          selectedFile={certificate}
          onChange={(event) => setCertificate(event.target.files?.[0] ?? null)}
        />
        {format === "pkcs12" ? (
          <label>
            <span>{t("ssl.clientCertificates.password")}</span>
            <input autoComplete="new-password" disabled={disabled} type="password" value={password} onChange={(event) => setPassword(event.target.value)} />
          </label>
        ) : (
          <CertificateFilePicker
            accept={format === "pem" ? ".pem,.key" : ".der,.key"}
            disabled={disabled}
            inputRef={privateKeyInputRef}
            label={t("ssl.clientCertificates.privateKey")}
            required
            selectedFile={privateKey}
            onChange={(event) => setPrivateKey(event.target.files?.[0] ?? null)}
          />
        )}
        <button className="primaryButton" disabled={disabled || certificate === null} type="submit">
          <Upload aria-hidden="true" size={14} />
          {t("ssl.clientCertificates.import")}
        </button>
      </form>

      <div className="sslClientCertificateList">
        {state.clientCertificates.length === 0 ? (
          <p>{t("ssl.clientCertificates.empty")}</p>
        ) : state.clientCertificates.map((identity) => (
          <article key={identity.id}>
            <FileKey2 aria-hidden="true" size={17} />
            <div>
              <strong>{identity.name}</strong>
              <span>{identity.locations.map((location) => location.host).join("、")}</span>
              <small title={identity.fingerprintSha256}>{identity.subject}</small>
            </div>
            <input
              aria-label={`${identity.name} ${t("ssl.clientCertificates.enabled")}`}
              checked={identity.enabled}
              disabled={disabled}
              type="checkbox"
              onChange={(event) => void updateClientCertificate(identity.id, {
                name: identity.name,
                enabled: event.target.checked,
                locations: identity.locations,
              })}
            />
            <button className="iconButton dangerTextButton" disabled={disabled} type="button" aria-label={t("ssl.clientCertificates.remove")} onClick={() => void removeClientCertificate(identity.id)}>
              <Trash2 aria-hidden="true" size={14} />
            </button>
          </article>
        ))}
      </div>
      <p className="sslHttpVersions">
        {t("ssl.clientCertificates.httpVersions")}: {state.supportedHttpVersions.join(" · ")}
      </p>
    </section>
  );
}
