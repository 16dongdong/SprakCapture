import { FileUp } from "lucide-react";
import { type ChangeEvent, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  maximumDescriptorEncodedCharacters,
  type ProtobufConfiguration,
  type ProtobufDescriptorUpload,
} from "../api/protocol";

interface ProtobufSchemaPanelProps {
  configuration: ProtobufConfiguration;
  disabled: boolean;
  upload(upload: ProtobufDescriptorUpload): Promise<ProtobufConfiguration | null>;
  onChange(configuration: ProtobufConfiguration): void;
}

/** 从文件读取 Data URL 的 Base64 段；浏览器原生编码避免展开大字节数组造成调用栈溢出。 */
function readDescriptorBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("error", () => reject(reader.error));
    reader.addEventListener("load", () => {
      const value = typeof reader.result === "string" ? reader.result : "";
      const separator = value.indexOf(",");
      if (separator < 0 || separator === value.length - 1) {
        reject(new Error("descriptorDataUrlInvalid"));
        return;
      }
      resolve(value.slice(separator + 1));
    });
    reader.readAsDataURL(file);
  });
}

/** 读取 descriptor 文件后交给控制接口登记；文件和 Base64 不写入 React 全局状态或持久化存储。 */
export function ProtobufSchemaPanel({
  configuration,
  disabled,
  upload,
  onChange,
}: ProtobufSchemaPanelProps) {
  const { t } = useTranslation();
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [descriptorName, setDescriptorName] = useState("");
  const [defaultMessageType, setDefaultMessageType] = useState("");
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);

  /** 接收文件选择器结果；超限文件在读入前拒绝，避免无意义的大内存分配。 */
  const selectDescriptor = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.item(0) ?? null;
    event.target.value = "";
    if (file === null) {
      return;
    }
    if (file.size === 0 || file.size > 16 * 1024 * 1024) {
      setSelectedFile(null);
      setUploadError(t("protocolSettings.upload.invalidSize"));
      return;
    }
    setSelectedFile(file);
    setDescriptorName(file.name.replace(/\.(desc|pb|bin)$/iu, ""));
    setDefaultMessageType("");
    setUploadError(null);
  };

  /** 上传已经选择的 FileDescriptorSet；成功后以服务端返回的配置替换局部草稿。 */
  const submitUpload = async () => {
    if (selectedFile === null || disabled || uploading) {
      return;
    }
    const name = descriptorName.trim();
    const messageType = defaultMessageType.trim();
    if (name.length === 0 || messageType.length === 0) {
      setUploadError(t("protocolSettings.upload.required"));
      return;
    }
    setUploading(true);
    setUploadError(null);
    try {
      const base64 = await readDescriptorBase64(selectedFile);
      if (base64.length > maximumDescriptorEncodedCharacters) {
        setUploadError(t("protocolSettings.upload.invalidSize"));
        return;
      }
      const nextConfiguration = await upload({
        name,
        defaultMessageType: messageType,
        base64,
      });
      if (nextConfiguration !== null) {
        onChange(nextConfiguration);
        setSelectedFile(null);
        setDescriptorName("");
        setDefaultMessageType("");
      }
    } catch {
      setUploadError(t("protocolSettings.upload.failed"));
    } finally {
      setUploading(false);
    }
  };

  const fieldsDisabled = disabled || uploading;
  return (
    <section className="protocolSettingsSection">
      <header>
        <FileUp aria-hidden="true" size={16} />
        <div>
          <h3>{t("protocolSettings.schemas.title")}</h3>
          <p>{t("protocolSettings.schemas.hint")}</p>
        </div>
      </header>
      {configuration.schemas.length === 0 ? (
        <p className="viewerNotice">{t("protocolSettings.schemas.empty")}</p>
      ) : (
        <ul className="protocolSchemaList">
          {configuration.schemas.map((schema) => (
            <li key={schema.id}>
              <strong>{schema.name}</strong>
              <span>{schema.defaultMessageType}</span>
            </li>
          ))}
        </ul>
      )}
      <div className="protocolUploadFields">
        <label className="fileChoiceButton">
          <FileUp aria-hidden="true" size={14} />
          <span>{selectedFile?.name ?? t("protocolSettings.upload.choose")}</span>
          <input
            accept=".desc,.pb,.bin,application/octet-stream"
            disabled={fieldsDisabled}
            type="file"
            onChange={selectDescriptor}
          />
        </label>
        <label>
          <span>{t("protocolSettings.upload.name")}</span>
          <input
            disabled={selectedFile === null || fieldsDisabled}
            required={selectedFile !== null}
            value={descriptorName}
            onChange={(event) => setDescriptorName(event.target.value)}
          />
        </label>
        <label>
          <span>{t("protocolSettings.upload.messageType")}</span>
          <input
            disabled={selectedFile === null || fieldsDisabled}
            placeholder="package.Message"
            required={selectedFile !== null}
            value={defaultMessageType}
            onChange={(event) => setDefaultMessageType(event.target.value)}
          />
        </label>
        <button
          className="primaryButton"
          disabled={selectedFile === null || fieldsDisabled}
          type="button"
          onClick={() => void submitUpload()}
        >
          <FileUp aria-hidden="true" size={14} />
          {uploading
            ? t("protocolSettings.upload.uploading")
            : t("protocolSettings.upload.action")}
        </button>
      </div>
      {uploadError !== null && (
        <p className="toolValidationMessage" role="alert">
          {uploadError}
        </p>
      )}
    </section>
  );
}
