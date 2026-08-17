import { Copy, KeyRound, RadioTower, UserRoundCog, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ManagementApiKeyResponse, ManagementIdentity, PublicConfiguration } from "../api/protocol";

interface MultiAccountSettingsProps {
  configuration: PublicConfiguration["multiAccount"];
  disabled: boolean;
  onConfigurationChange(configuration: PublicConfiguration["multiAccount"]): void;
  getIdentity(): Promise<ManagementIdentity | null>;
  updateIdentity(username: string, password: string): Promise<ManagementApiKeyResponse | null>;
  getApiKey(): Promise<ManagementApiKeyResponse | null>;
}

/**
 * 渲染唯一远程管理入口、共享管理员身份和 Key 操作区；完整 Key 只存在于结果对话框状态。
 *
 * 运行上下文：设置页传入脱敏快照与控制动作；读取或写入失败时保留表单，完整 Key 在关闭或卸载时清空。
 */
export function MultiAccountSettings({
  configuration,
  disabled,
  onConfigurationChange,
  getIdentity,
  updateIdentity,
  getApiKey,
}: MultiAccountSettingsProps) {
  const { t } = useTranslation();
  const [identity, setIdentity] = useState<ManagementIdentity | null>(null);
  const [newUsername, setNewUsername] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [secretResponse, setSecretResponse] = useState<ManagementApiKeyResponse | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let active = true;
    if (configuration.state !== "running") {
      setIdentity(null);
      setNewUsername("");
      return () => {
        active = false;
      };
    }
    void getIdentity().then((result) => {
      if (!active) {
        return;
      }
      setIdentity(result);
      setNewUsername(result?.username ?? "");
    });
    return () => {
      active = false;
      setSecretResponse(null);
    };
  }, [configuration.state, getIdentity]);

  /** 保存新管理员账号和密码；账号服务在同一事务中让派生 Key 随新凭据立即变化。 */
  const submitIdentity = async () => {
    const response = await updateIdentity(newUsername, newPassword);
    if (response === null) {
      return;
    }
    setIdentity(response.identity);
    setNewUsername(response.identity.username);
    setNewPassword("");
    setSecretResponse(response);
    setCopied(false);
  };

  /**
   * 直接读取由当前管理员凭据确定性派生的完整 Key。
   *
   * 运行上下文：仅由用户点击触发，响应停留在结果对话框内存；控制面请求失败时返回 null，页面不展示过期内容。
   */
  const submitKeyRequest = async () => {
    const response = await getApiKey();
    if (response === null) {
      return;
    }
    setIdentity(response.identity);
    setSecretResponse(response);
    setCopied(false);
  };

  /** 复制当前短生命周期 Key；剪贴板拒绝会直接传播，不显示伪造成功状态。 */
  const copySecret = async () => {
    if (secretResponse === null) {
      return;
    }
    await navigator.clipboard.writeText(secretResponse.apiKey);
    setCopied(true);
  };

  /** 关闭结果框并移除完整 Key 的组件引用，后续渲染不再包含秘密值。 */
  const closeSecret = () => {
    setSecretResponse(null);
    setCopied(false);
  };

  const managementReady = configuration.state === "running";
  return (
    <div className="multiAccountSettings" aria-label={t("page.settings.multiAccountGroup")}>
      <section className="multiAccountPanel">
        <h2><RadioTower aria-hidden="true" size={16} /> {t("page.settings.multiAccountGroup")}</h2>
        <div className="multiAccountGrid">
          <label className="settingsCheckboxRow multiAccountSpanTwo">
            <input checked={configuration.enabled} disabled={disabled} type="checkbox" onChange={(event) => onConfigurationChange({ ...configuration, enabled: event.target.checked })} />
            <span><strong>{t("page.settings.multiAccountEnabled")}</strong><small>{t("page.settings.multiAccountEnabledHint")}</small></span>
          </label>
          <label><span>{t("page.settings.multiAccountHost")}</span><input disabled={disabled || !configuration.enabled} required value={configuration.remoteHost} onChange={(event) => onConfigurationChange({ ...configuration, remoteHost: event.target.value })} /></label>
          <label><span>{t("page.settings.multiAccountPort")}</span><input disabled={disabled || !configuration.enabled} required max={65535} min={1} type="number" value={configuration.remotePort} onChange={(event) => onConfigurationChange({ ...configuration, remotePort: event.target.valueAsNumber })} /></label>
          <label><span>{t("page.settings.multiAccountState")}</span><input readOnly value={t(`page.settings.multiAccountState_${configuration.state}`)} /></label>
          <label><span>{t("page.settings.multiAccountKeyPrefix")}</span><input readOnly value={configuration.apiKeyPrefix ?? ""} /></label>
        </div>
        {configuration.error ? <p className="settingsRestartHint">{configuration.error}</p> : null}
      </section>

      {/* 管理身份是独立按钮提交的子操作，输入框不能参与外层“应用配置”的 required 校验。 */}
      {managementReady ? <section className="multiAccountPanel"><h2><UserRoundCog aria-hidden="true" size={16} /> {t("page.settings.managementIdentityGroup")}</h2><p className="multiAccountHint">{t("page.settings.managementIdentityWarning")}</p><div className="multiAccountGrid"><label><span>{t("page.settings.managementCurrentUsername")}</span><input readOnly value={identity?.username ?? ""} /></label><label><span>{t("page.settings.managementNewUsername")}</span><input autoComplete="username" disabled={disabled} value={newUsername} onChange={(event) => setNewUsername(event.target.value)} /></label><label className="multiAccountSpanTwo"><span>{t("page.settings.managementNewPassword")}</span><input autoComplete="new-password" disabled={disabled} type="password" value={newPassword} onChange={(event) => setNewPassword(event.target.value)} /></label><button className="multiAccountSpanTwo" disabled={disabled || newUsername.length === 0 || newPassword.length === 0} type="button" onClick={() => void submitIdentity()}>{t("page.settings.managementUpdateIdentity")}</button></div></section> : null}

      {managementReady ? <section className="multiAccountPanel"><h2><KeyRound aria-hidden="true" size={16} /> {t("page.settings.managementApiKeyGroup")}</h2><p className="multiAccountHint">{t("page.settings.managementApiKeyHint")}</p><button className="multiAccountFullButton" disabled={disabled} type="button" onClick={() => void submitKeyRequest()}>{t("page.settings.managementGetApiKey")}</button></section> : null}

      {secretResponse !== null ? <div className="dialogBackdrop" role="presentation"><section aria-modal="true" className="confirmDialog" role="dialog"><header className="confirmDialogHeader"><h2>{t("page.settings.managementApiKeyResult")}</h2><button aria-label={t("page.settings.managementCloseSecret")} type="button" onClick={closeSecret}><X size={16} /></button></header><div className="confirmDialogBody"><p>{t("page.settings.managementApiKeyOneTimeHint")}</p><label><span>{t("page.settings.managementFullApiKey")}</span><input readOnly value={secretResponse.apiKey} /></label>{copied ? <p role="status">{t("page.settings.managementApiKeyCopied")}</p> : null}</div><footer className="confirmDialogActions"><button type="button" onClick={() => void copySecret()}><Copy aria-hidden="true" size={16} /> {t("page.settings.managementCopyApiKey")}</button><button className="primaryButton" type="button" onClick={closeSecret}>{t("page.settings.managementDone")}</button></footer></section></div> : null}
    </div>
  );
}
