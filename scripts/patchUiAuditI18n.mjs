/**
 * Patch all Web locale catalogs with UI-audit fix keys.
 * Keys must stay identical across locales for checkI18n.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const localesDir = join(root, "Server/Frontend/Web/src/locales");
const locales = [
  "en",
  "zh-Hans",
  "zh-Hant",
  "ja",
  "ko",
  "es",
  "fr",
  "de",
  "pt-BR",
  "ru",
];

/** @type {Record<string, Record<string, string>>} */
const patches = {
  en: {
    "page.settings.back": "Back",
    "page.settings.descriptionInterface": "Choose the interface language for this console.",
    "page.settings.descriptionListener": "Configure the SOCKS5 listen address and UDP bind policy.",
    "page.settings.descriptionAuthentication": "Choose whether clients must authenticate with a username and password.",
    "page.settings.descriptionCapacity": "Tune connection limits, timeouts, and relay buffers.",
    "page.settings.capacityConnections": "Connections",
    "page.settings.capacityTimeouts": "Timeouts",
    "page.settings.capacityBuffers": "Buffers",
    "listeners.reverse.description":
      "Configure local reverse HTTP proxy listeners. Applying rules restarts the service and disconnects active proxy connections.",
    "listeners.forward.description":
      "Configure TCP port-forward listeners. Applying rules restarts the service and disconnects active proxy connections.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf decoding is disabled. Enable it in Protocol tools.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "The message body was truncated and cannot be decoded as Protobuf.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "No Protobuf route matched this message.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "The descriptor set for this route is unavailable.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "The configured message type was not found in the descriptor set.",
    "viewer.protocol.protobuf.errors.unknown":
      "Protobuf decoding failed for an unknown reason.",
    "transactions.navigator.sequence": "Sequence",
    "transactions.navigator.sequenceLabel": "Transaction sequence",
    "transactions.navigator.viewMode": "Navigator view",
  },
  "zh-Hans": {
    "page.settings.back": "返回",
    "page.settings.descriptionInterface": "选择本控制台的界面语言。",
    "page.settings.descriptionListener": "配置 SOCKS5 监听地址与 UDP 绑定策略。",
    "page.settings.descriptionAuthentication": "选择客户端是否需要用户名与密码认证。",
    "page.settings.descriptionCapacity": "调整连接上限、超时与转发缓冲区。",
    "page.settings.capacityConnections": "连接",
    "page.settings.capacityTimeouts": "超时",
    "page.settings.capacityBuffers": "缓冲区",
    "listeners.reverse.description":
      "配置本地反向 HTTP 代理监听器。应用规则会重启服务并断开当前代理连接。",
    "listeners.forward.description":
      "配置 TCP 端口转发监听器。应用规则会重启服务并断开当前代理连接。",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf 解码未启用。请在「协议工具」中开启。",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "报文正文已截断，无法按 Protobuf 解码。",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "没有匹配此报文的 Protobuf 路由。",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "此路由对应的描述符集不可用。",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "描述符集中找不到已配置的消息类型。",
    "viewer.protocol.protobuf.errors.unknown": "Protobuf 解码因未知原因失败。",
    "transactions.navigator.sequence": "序列",
    "transactions.navigator.sequenceLabel": "事务序列",
    "transactions.navigator.viewMode": "导航视图",
  },
  "zh-Hant": {
    "page.settings.back": "返回",
    "page.settings.descriptionInterface": "選擇本主控台的介面語言。",
    "page.settings.descriptionListener": "設定 SOCKS5 監聽位址與 UDP 繫結策略。",
    "page.settings.descriptionAuthentication": "選擇用戶端是否需要使用者名稱與密碼驗證。",
    "page.settings.descriptionCapacity": "調整連線上限、逾時與轉送緩衝區。",
    "page.settings.capacityConnections": "連線",
    "page.settings.capacityTimeouts": "逾時",
    "page.settings.capacityBuffers": "緩衝區",
    "listeners.reverse.description":
      "設定本機反向 HTTP 代理監聽器。套用規則會重新啟動服務並中斷目前代理連線。",
    "listeners.forward.description":
      "設定 TCP 連接埠轉送監聽器。套用規則會重新啟動服務並中斷目前代理連線。",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf 解碼未啟用。請在「協定工具」中開啟。",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "訊息本文已截斷，無法以 Protobuf 解碼。",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "沒有符合此訊息的 Protobuf 路由。",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "此路由對應的描述符集無法使用。",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "描述符集中找不到已設定的訊息類型。",
    "viewer.protocol.protobuf.errors.unknown": "Protobuf 解碼因未知原因失敗。",
    "transactions.navigator.sequence": "序列",
    "transactions.navigator.sequenceLabel": "交易序列",
    "transactions.navigator.viewMode": "導覽檢視",
  },
  ja: {
    "page.settings.back": "戻る",
    "page.settings.descriptionInterface": "このコンソールの表示言語を選択します。",
    "page.settings.descriptionListener": "SOCKS5 の待ち受けアドレスと UDP バインド方針を設定します。",
    "page.settings.descriptionAuthentication": "クライアントにユーザー名とパスワード認証が必要かを選びます。",
    "page.settings.descriptionCapacity": "接続上限、タイムアウト、中継バッファを調整します。",
    "page.settings.capacityConnections": "接続",
    "page.settings.capacityTimeouts": "タイムアウト",
    "page.settings.capacityBuffers": "バッファ",
    "listeners.reverse.description":
      "ローカルのリバース HTTP プロキシリスナーを設定します。規則の適用でサービスが再起動し、現行のプロキシ接続は切断されます。",
    "listeners.forward.description":
      "TCP ポート転送リスナーを設定します。規則の適用でサービスが再起動し、現行のプロキシ接続は切断されます。",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf デコードは無効です。「プロトコルツール」で有効にしてください。",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "メッセージ本体が切り詰められており、Protobuf としてデコードできません。",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "このメッセージに一致する Protobuf ルートがありません。",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "このルートの記述子セットを利用できません。",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "設定したメッセージ型が記述子セットに見つかりません。",
    "viewer.protocol.protobuf.errors.unknown": "Protobuf デコードが不明な理由で失敗しました。",
    "transactions.navigator.sequence": "シーケンス",
    "transactions.navigator.sequenceLabel": "トランザクションシーケンス",
    "transactions.navigator.viewMode": "ナビゲーター表示",
  },
  ko: {
    "page.settings.back": "뒤로",
    "page.settings.descriptionInterface": "이 콘솔의 인터페이스 언어를 선택합니다.",
    "page.settings.descriptionListener": "SOCKS5 수신 주소와 UDP 바인드 정책을 구성합니다.",
    "page.settings.descriptionAuthentication": "클라이언트가 사용자 이름과 비밀번호로 인증해야 하는지 선택합니다.",
    "page.settings.descriptionCapacity": "연결 한도, 시간 제한, 중계 버퍼를 조정합니다.",
    "page.settings.capacityConnections": "연결",
    "page.settings.capacityTimeouts": "시간 제한",
    "page.settings.capacityBuffers": "버퍼",
    "listeners.reverse.description":
      "로컬 리버스 HTTP 프록시 리스너를 구성합니다. 규칙을 적용하면 서비스가 다시 시작되고 활성 프록시 연결이 끊깁니다.",
    "listeners.forward.description":
      "TCP 포트 전달 리스너를 구성합니다. 규칙을 적용하면 서비스가 다시 시작되고 활성 프록시 연결이 끊깁니다.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf 디코딩이 비활성화되어 있습니다. 프로토콜 도구에서 활성화하세요.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "메시지 본문이 잘려 Protobuf로 디코딩할 수 없습니다.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "이 메시지와 일치하는 Protobuf 경로가 없습니다.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "이 경로의 디스크립터 집합을 사용할 수 없습니다.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "구성된 메시지 유형을 디스크립터 집합에서 찾을 수 없습니다.",
    "viewer.protocol.protobuf.errors.unknown": "알 수 없는 이유로 Protobuf 디코딩에 실패했습니다.",
    "transactions.navigator.sequence": "시퀀스",
    "transactions.navigator.sequenceLabel": "트랜잭션 시퀀스",
    "transactions.navigator.viewMode": "탐색기 보기",
  },
  es: {
    "page.settings.back": "Volver",
    "page.settings.descriptionInterface": "Elija el idioma de la interfaz de esta consola.",
    "page.settings.descriptionListener": "Configure la dirección de escucha SOCKS5 y la política de enlace UDP.",
    "page.settings.descriptionAuthentication": "Elija si los clientes deben autenticarse con usuario y contraseña.",
    "page.settings.descriptionCapacity": "Ajuste los límites de conexión, los tiempos de espera y los búferes de retransmisión.",
    "page.settings.capacityConnections": "Conexiones",
    "page.settings.capacityTimeouts": "Tiempos de espera",
    "page.settings.capacityBuffers": "Búferes",
    "listeners.reverse.description":
      "Configure los oyentes de proxy HTTP inverso local. Al aplicar las reglas se reinicia el servicio y se desconectan las conexiones de proxy activas.",
    "listeners.forward.description":
      "Configure los oyentes de reenvío de puertos TCP. Al aplicar las reglas se reinicia el servicio y se desconectan las conexiones de proxy activas.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "La decodificación Protobuf está desactivada. Actívela en Herramientas de protocolo.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "El cuerpo del mensaje se truncó y no se puede decodificar como Protobuf.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "Ninguna ruta Protobuf coincide con este mensaje.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "El conjunto de descriptores de esta ruta no está disponible.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "El tipo de mensaje configurado no se encontró en el conjunto de descriptores.",
    "viewer.protocol.protobuf.errors.unknown":
      "La decodificación Protobuf falló por un motivo desconocido.",
    "transactions.navigator.sequence": "Secuencia",
    "transactions.navigator.sequenceLabel": "Secuencia de transacciones",
    "transactions.navigator.viewMode": "Vista del navegador",
  },
  fr: {
    "page.settings.back": "Retour",
    "page.settings.descriptionInterface": "Choisissez la langue de l’interface de cette console.",
    "page.settings.descriptionListener": "Configurez l’adresse d’écoute SOCKS5 et la politique de liaison UDP.",
    "page.settings.descriptionAuthentication": "Indiquez si les clients doivent s’authentifier avec un nom d’utilisateur et un mot de passe.",
    "page.settings.descriptionCapacity": "Réglez les limites de connexion, les délais et les tampons de relais.",
    "page.settings.capacityConnections": "Connexions",
    "page.settings.capacityTimeouts": "Délais",
    "page.settings.capacityBuffers": "Tampons",
    "listeners.reverse.description":
      "Configurez les écouteurs de proxy HTTP inverse local. L’application des règles redémarre le service et déconnecte les connexions proxy actives.",
    "listeners.forward.description":
      "Configurez les écouteurs de redirection de port TCP. L’application des règles redémarre le service et déconnecte les connexions proxy actives.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Le décodage Protobuf est désactivé. Activez-le dans Outils de protocole.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "Le corps du message a été tronqué et ne peut pas être décodé en Protobuf.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "Aucune route Protobuf ne correspond à ce message.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "L’ensemble de descripteurs de cette route est indisponible.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "Le type de message configuré est introuvable dans l’ensemble de descripteurs.",
    "viewer.protocol.protobuf.errors.unknown":
      "Le décodage Protobuf a échoué pour une raison inconnue.",
    "transactions.navigator.sequence": "Séquence",
    "transactions.navigator.sequenceLabel": "Séquence des transactions",
    "transactions.navigator.viewMode": "Vue du navigateur",
  },
  de: {
    "page.settings.back": "Zurück",
    "page.settings.descriptionInterface": "Wählen Sie die Oberflächen­sprache dieser Konsole.",
    "page.settings.descriptionListener": "Konfigurieren Sie die SOCKS5-Listenadresse und die UDP-Bindungs­richtlinie.",
    "page.settings.descriptionAuthentication": "Legen Sie fest, ob Clients sich mit Benutzername und Passwort authentifizieren müssen.",
    "page.settings.descriptionCapacity": "Passen Sie Verbindungslimits, Timeouts und Relay-Puffer an.",
    "page.settings.capacityConnections": "Verbindungen",
    "page.settings.capacityTimeouts": "Timeouts",
    "page.settings.capacityBuffers": "Puffer",
    "listeners.reverse.description":
      "Konfigurieren Sie lokale Reverse-HTTP-Proxy-Listener. Das Anwenden der Regeln startet den Dienst neu und trennt aktive Proxy-Verbindungen.",
    "listeners.forward.description":
      "Konfigurieren Sie TCP-Portweiterleitungs-Listener. Das Anwenden der Regeln startet den Dienst neu und trennt aktive Proxy-Verbindungen.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Protobuf-Dekodierung ist deaktiviert. Aktivieren Sie sie unter Protokoll­werkzeuge.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "Der Nachrichtenkörper wurde gekürzt und kann nicht als Protobuf dekodiert werden.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "Keine Protobuf-Route passt zu dieser Nachricht.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "Der Deskriptorsatz für diese Route ist nicht verfügbar.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "Der konfigurierte Nachrichtentyp wurde im Deskriptorsatz nicht gefunden.",
    "viewer.protocol.protobuf.errors.unknown":
      "Protobuf-Dekodierung ist aus unbekanntem Grund fehlgeschlagen.",
    "transactions.navigator.sequence": "Sequenz",
    "transactions.navigator.sequenceLabel": "Transaktionssequenz",
    "transactions.navigator.viewMode": "Navigatoransicht",
  },
  "pt-BR": {
    "page.settings.back": "Voltar",
    "page.settings.descriptionInterface": "Escolha o idioma da interface deste console.",
    "page.settings.descriptionListener": "Configure o endereço de escuta SOCKS5 e a política de vínculo UDP.",
    "page.settings.descriptionAuthentication": "Escolha se os clientes devem autenticar com nome de usuário e senha.",
    "page.settings.descriptionCapacity": "Ajuste limites de conexão, tempos limite e buffers de retransmissão.",
    "page.settings.capacityConnections": "Conexões",
    "page.settings.capacityTimeouts": "Tempos limite",
    "page.settings.capacityBuffers": "Buffers",
    "listeners.reverse.description":
      "Configure ouvintes de proxy HTTP reverso local. Aplicar as regras reinicia o serviço e desconecta conexões de proxy ativas.",
    "listeners.forward.description":
      "Configure ouvintes de encaminhamento de porta TCP. Aplicar as regras reinicia o serviço e desconecta conexões de proxy ativas.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "A decodificação Protobuf está desativada. Ative-a em Ferramentas de protocolo.",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "O corpo da mensagem foi truncado e não pode ser decodificado como Protobuf.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "Nenhuma rota Protobuf corresponde a esta mensagem.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "O conjunto de descritores desta rota está indisponível.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "O tipo de mensagem configurado não foi encontrado no conjunto de descritores.",
    "viewer.protocol.protobuf.errors.unknown":
      "A decodificação Protobuf falhou por um motivo desconhecido.",
    "transactions.navigator.sequence": "Sequência",
    "transactions.navigator.sequenceLabel": "Sequência de transações",
    "transactions.navigator.viewMode": "Visualização do navegador",
  },
  ru: {
    "page.settings.back": "Назад",
    "page.settings.descriptionInterface": "Выберите язык интерфейса этой консоли.",
    "page.settings.descriptionListener": "Настройте адрес прослушивания SOCKS5 и политику привязки UDP.",
    "page.settings.descriptionAuthentication": "Укажите, должны ли клиенты проходить проверку имени пользователя и пароля.",
    "page.settings.descriptionCapacity": "Настройте лимиты подключений, тайм-ауты и буферы ретрансляции.",
    "page.settings.capacityConnections": "Подключения",
    "page.settings.capacityTimeouts": "Тайм-ауты",
    "page.settings.capacityBuffers": "Буферы",
    "listeners.reverse.description":
      "Настройте локальные слушатели обратного HTTP-прокси. Применение правил перезапускает службу и разрывает активные прокси-соединения.",
    "listeners.forward.description":
      "Настройте слушатели переадресации TCP-портов. Применение правил перезапускает службу и разрывает активные прокси-соединения.",
    "viewer.protocol.protobuf.errors.protobufDisabled":
      "Декодирование Protobuf отключено. Включите его в «Инструментах протокола».",
    "viewer.protocol.protobuf.errors.bodyTruncated":
      "Тело сообщения обрезано и не может быть декодировано как Protobuf.",
    "viewer.protocol.protobuf.errors.protobufRouteNotFound":
      "Ни один маршрут Protobuf не соответствует этому сообщению.",
    "viewer.protocol.protobuf.errors.descriptorUnavailable":
      "Набор дескрипторов для этого маршрута недоступен.",
    "viewer.protocol.protobuf.errors.messageTypeNotFound":
      "Настроенный тип сообщения не найден в наборе дескрипторов.",
    "viewer.protocol.protobuf.errors.unknown":
      "Декодирование Protobuf не удалось по неизвестной причине.",
    "transactions.navigator.sequence": "Последовательность",
    "transactions.navigator.sequenceLabel": "Последовательность транзакций",
    "transactions.navigator.viewMode": "Вид навигатора",
  },
};

function setPath(object, path, value) {
  const parts = path.split(".");
  let cursor = object;
  for (let index = 0; index < parts.length - 1; index += 1) {
    const part = parts[index];
    if (cursor[part] === undefined || typeof cursor[part] !== "object") {
      cursor[part] = {};
    }
    cursor = cursor[part];
  }
  cursor[parts[parts.length - 1]] = value;
}

for (const locale of locales) {
  const catalogPath = join(localesDir, locale, "app.json");
  const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
  const entries = patches[locale] ?? patches.en;
  for (const [path, value] of Object.entries(entries)) {
    setPath(catalog, path, value);
  }
  writeFileSync(catalogPath, `${JSON.stringify(catalog, null, 2)}\n`, "utf8");
  console.log(`patched ${locale}`);
}
