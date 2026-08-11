import { Plus, Route, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  type LocationPattern,
  type ProtobufConfiguration,
  type ProtobufRoute,
} from "../api/protocol";

interface ProtobufRouteEditorProps {
  configuration: ProtobufConfiguration;
  disabled: boolean;
  onChange(configuration: ProtobufConfiguration): void;
}

interface RouteFieldsProps {
  route: ProtobufRoute;
  schemas: ProtobufConfiguration["schemas"];
  disabled: boolean;
  onChange(route: ProtobufRoute): void;
}

/** 创建空的路由位置；空字段沿用统一 Location 协议的任意匹配语义。 */
function createLocation(): LocationPattern {
  return { protocol: "", host: "", port: "", path: "", query: null };
}

/** 为新路由生成在当前草稿中唯一的稳定标识；标识只用于配置引用，不依赖浏览器随机 API。 */
function createRouteId(routes: ProtobufRoute[]): string {
  let ordinal = routes.length + 1;
  while (routes.some((route) => route.id === `protobuf-route-${ordinal}`)) {
    ordinal += 1;
  }
  return `protobuf-route-${ordinal}`;
}

/** 更新位置条件中的单一文本字段；空 query 规范化为 null 以匹配控制契约。 */
function updateLocation(
  route: ProtobufRoute,
  field: keyof LocationPattern,
  value: string,
): ProtobufRoute {
  return {
    ...route,
    location: {
      ...route.location,
      [field]: field === "query" && value === "" ? null : value,
    },
  };
}

/** 渲染一条路由的全部结构化字段；schema 选择变化时同步填入该描述符的默认请求类型。 */
function RouteFields({ route, schemas, disabled, onChange }: RouteFieldsProps) {
  const { t } = useTranslation();
  return (
    <div className="protocolRouteGrid">
      <label>
        <span>{t("protocolSettings.routes.id")}</span>
        <input
          disabled={disabled}
          required
          value={route.id}
          onChange={(event) => onChange({ ...route, id: event.target.value })}
        />
      </label>
      <label>
        <span>{t("protocolSettings.routes.schema")}</span>
        <select
          disabled={disabled}
          value={route.schemaId}
          onChange={(event) => {
            const schema = schemas.find((entry) => entry.id === event.target.value);
            onChange({
              ...route,
              schemaId: event.target.value,
              messageType: schema?.defaultMessageType ?? route.messageType,
            });
          }}
        >
          <option value="">{t("protocolSettings.routes.selectSchema")}</option>
          {schemas.map((schema) => (
            <option key={schema.id} value={schema.id}>
              {schema.name}
            </option>
          ))}
        </select>
      </label>
      {(["protocol", "host", "port", "path", "query"] as const).map((field) => (
        <label key={field}>
          <span>{t(`tools.form.${field}`)}</span>
          <input
            disabled={disabled}
            value={route.location[field] ?? ""}
            onChange={(event) => onChange(updateLocation(route, field, event.target.value))}
          />
        </label>
      ))}
      <label>
        <span>{t("protocolSettings.routes.requestType")}</span>
        <input
          disabled={disabled}
          required
          value={route.messageType}
          onChange={(event) => onChange({ ...route, messageType: event.target.value })}
        />
      </label>
      <label>
        <span>{t("protocolSettings.routes.responseType")}</span>
        <input
          disabled={disabled}
          value={route.responseMessageType ?? ""}
          onChange={(event) =>
            onChange({
              ...route,
              responseMessageType:
                event.target.value === "" ? null : event.target.value,
            })
          }
        />
      </label>
    </div>
  );
}

/** 渲染 Protobuf 位置路由表；新增、删除与字段编辑都只更新局部草稿，确认后才写入控制面。 */
export function ProtobufRouteEditor({
  configuration,
  disabled,
  onChange,
}: ProtobufRouteEditorProps) {
  const { t } = useTranslation();

  /** 将一条已编辑路由写回复制后的数组，避免直接修改配置快照。 */
  const replaceRoute = (index: number, nextRoute: ProtobufRoute) => {
    onChange({
      ...configuration,
      routes: configuration.routes.map((route, routeIndex) =>
        routeIndex === index ? nextRoute : route,
      ),
    });
  };

  /** 基于已登记的最后一个描述符添加路由，默认消息类型来自该 descriptor 元数据。 */
  const addRoute = () => {
    const schema = configuration.schemas.at(-1);
    if (schema === undefined) {
      return;
    }
    onChange({
      ...configuration,
      routes: [
        ...configuration.routes,
        {
          id: createRouteId(configuration.routes),
          location: createLocation(),
          messageType: schema.defaultMessageType,
          responseMessageType: null,
          schemaId: schema.id,
        },
      ],
    });
  };

  /** 删除草稿中的指定路由；描述符登记不受影响，避免把配置更新误当作文件删除。 */
  const removeRoute = (index: number) => {
    onChange({
      ...configuration,
      routes: configuration.routes.filter((_, routeIndex) => routeIndex !== index),
    });
  };

  return (
    <section className="protocolSettingsSection">
      <header>
        <Route aria-hidden="true" size={16} />
        <div>
          <h3>{t("protocolSettings.routes.title")}</h3>
          <p>{t("protocolSettings.routes.hint")}</p>
        </div>
        <button
          disabled={disabled || configuration.schemas.length === 0}
          type="button"
          onClick={addRoute}
        >
          <Plus aria-hidden="true" size={14} />
          {t("protocolSettings.routes.add")}
        </button>
      </header>
      {configuration.routes.length === 0 ? (
        <p className="viewerNotice">{t("protocolSettings.routes.empty")}</p>
      ) : (
        <div className="protocolRouteList">
          {configuration.routes.map((route, index) => (
            // 路由标识本身可编辑；使用索引保证每次键入不会因 key 改变卸载输入框并丢失焦点。
            <fieldset className="protocolRoute" key={index}>
              <legend>{t("protocolSettings.routes.rule", { index: index + 1 })}</legend>
              <button
                aria-label={t("protocolSettings.routes.remove", { index: index + 1 })}
                className="iconButton"
                disabled={disabled}
                type="button"
                onClick={() => removeRoute(index)}
              >
                <Trash2 aria-hidden="true" size={14} />
              </button>
              <RouteFields
                disabled={disabled}
                route={route}
                schemas={configuration.schemas}
                onChange={(nextRoute) => replaceRoute(index, nextRoute)}
              />
            </fieldset>
          ))}
        </div>
      )}
    </section>
  );
}
