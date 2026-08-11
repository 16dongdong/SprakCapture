import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const supportedLocales = [
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
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "../../../..");

/**
 * 读取并解析受版本控制的 UTF-8 目录文件；编码或语法错误直接携带路径终止检查。
 *
 * 运行上下文：提交前统一检查前端、后端和 MCP 三套编译期目录；MCP 严格解析器额外要求无 BOM，既有目录按各自运行时兼容范围读取。
 * 参数：`catalogPath` 是仓库内 JSON 目录的绝对路径；`requireCanonicalUtf8` 为真时拒绝 UTF-8 BOM。
 * 失败语义：严格模式遇到 BOM 或任一模式遇到 JSON 语法异常时抛出带路径错误。
 */
async function readCatalog(catalogPath, requireCanonicalUtf8 = false) {
  const source = await readFile(catalogPath, "utf8");
  if (requireCanonicalUtf8 && source.startsWith("\uFEFF")) {
    throw new Error(`语言目录必须使用无 BOM UTF-8 编码：${catalogPath}`);
  }
  try {
    return JSON.parse(source.replace(/^\uFEFF/, ""));
  } catch (error) {
    throw new Error(`语言目录 JSON 无效：${catalogPath}`, { cause: error });
  }
}

/**
 * 把嵌套前端目录展开为稳定点分键；叶子必须是非空字符串，禁止对象或空值进入运行时。
 */
function flattenCatalog(catalog, prefix = "", flattened = new Map()) {
  for (const [name, value] of Object.entries(catalog)) {
    const key = prefix ? `${prefix}.${name}` : name;
    if (typeof value === "string") {
      if (!value.trim()) {
        throw new Error(`语言目录包含空文案：${key}`);
      }
      flattened.set(key, value);
      continue;
    }
    if (value === null || Array.isArray(value) || typeof value !== "object") {
      throw new Error(`语言目录叶子不是字符串：${key}`);
    }
    flattenCatalog(value, key, flattened);
  }
  return flattened;
}

/**
 * 提取模板占位符并排序；前端使用双花括号，后端错误目录使用单花括号。
 */
function collectPlaceholders(message, placeholderPattern) {
  return [...message.matchAll(placeholderPattern)]
    .map((match) => match[1])
    .sort();
}

/**
 * 对照英文基线验证键集合与占位符；缺键、额外键或参数漂移均阻止构建。
 */
function compareCatalogs(reference, candidate, locale, placeholderPattern) {
  const referenceKeys = [...reference.keys()].sort();
  const candidateKeys = [...candidate.keys()].sort();
  const missingKeys = referenceKeys.filter((key) => !candidate.has(key));
  const extraKeys = candidateKeys.filter((key) => !reference.has(key));
  const placeholderMismatches = referenceKeys.filter((key) => {
    if (!candidate.has(key)) {
      return false;
    }
    const referenceParams = collectPlaceholders(
      reference.get(key),
      placeholderPattern,
    );
    const candidateParams = collectPlaceholders(
      candidate.get(key),
      placeholderPattern,
    );
    return referenceParams.join("\0") !== candidateParams.join("\0");
  });
  if (missingKeys.length || extraKeys.length || placeholderMismatches.length) {
    throw new Error(
      [
        `${locale} 语言目录与英文基线不一致`,
        `缺少：${missingKeys.join(", ") || "无"}`,
        `额外：${extraKeys.join(", ") || "无"}`,
        `占位符漂移：${placeholderMismatches.join(", ") || "无"}`,
      ].join("\n"),
    );
  }
}

/**
 * 校验一个十语目录族的键集合、占位符和编码契约。
 *
 * 运行上下文：由提交前国际化检查依次校验前端、后端和 MCP；各目录族可独立声明是否要求无 BOM UTF-8。
 * 参数：路径解析器定位语言文件，`placeholderPattern` 识别参数，`forbiddenPlaceholders` 禁止不稳定参数，`requireCanonicalUtf8` 控制 BOM 契约。
 * 失败语义：任一语言缺键、占位符不一致、包含禁用参数或违反编码要求时抛出错误并终止检查。
 */
async function checkCatalogFamily(
  resolveCatalogPath,
  placeholderPattern,
  forbiddenPlaceholders = [],
  requireCanonicalUtf8 = false,
) {
  const reference = flattenCatalog(
    await readCatalog(resolveCatalogPath("en"), requireCanonicalUtf8),
  );
  for (const locale of supportedLocales) {
    const candidate = flattenCatalog(
      await readCatalog(resolveCatalogPath(locale), requireCanonicalUtf8),
    );
    compareCatalogs(reference, candidate, locale, placeholderPattern);
    const forbiddenKeys = [...candidate.entries()]
      .filter(([, message]) =>
        collectPlaceholders(message, placeholderPattern).some((placeholder) =>
          forbiddenPlaceholders.includes(placeholder),
        ),
      )
      .map(([key]) => key);
    if (forbiddenKeys.length) {
      throw new Error(
        `${locale} 语言目录包含禁止进入 message 的占位符：${forbiddenKeys.join(", ")}`,
      );
    }
  }
  return reference.size;
}

const frontendKeyCount = await checkCatalogFamily(
  (locale) =>
    resolve(
      repositoryRoot,
      `Server/Frontend/Web/src/locales/${locale}/app.json`,
    ),
  /\{\{([A-Za-z][A-Za-z0-9]*)\}\}/g,
);
const backendKeyCount = await checkCatalogFamily(
  (locale) =>
    resolve(repositoryRoot, `Server/Backend/locales/${locale}/errors.json`),
  /(?<!\{)\{([A-Za-z][A-Za-z0-9]*)\}(?!\})/g,
  ["detail"],
);
const mcpKeyCount = await checkCatalogFamily(
  (locale) =>
    resolve(repositoryRoot, `Server/Mcp/locales/${locale}/messages.json`),
  /(?<!\{)\{([A-Za-z][A-Za-z0-9]*)\}(?!\})/g,
  [],
  true,
);

console.log(
  `国际化目录检查通过：10 种语言，前端 ${frontendKeyCount} 个键，后端 ${backendKeyCount} 个错误键，MCP ${mcpKeyCount} 个键。`,
);
