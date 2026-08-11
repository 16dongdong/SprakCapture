use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State, multipart::MultipartRejection},
    routing::post,
};
use serde::Serialize;
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use super::{ApiError, ControlState, ErrorCode, LocalizedApiError, RequestLocale};

const maximumImportBodyBytes: usize = 512 * 1024 * 1024;
const maximumImportedFileBytes: u64 = 64 * 1024 * 1024;
const maximumImportedTotalBytes: u64 = 480 * 1024 * 1024;
const maximumImportedFiles: usize = 2_000;
const maximumRelativePathBytes: usize = 4_096;
const importDirectoryName: &str = "imports";

/// 返回浏览器已导入到受管映射根内的相对路径；规则可直接保存该路径，且无需接触浏览器隐藏的绝对路径。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MapLocalImportResult {
    localPath: String,
    fileCount: usize,
    totalBytes: u64,
}

/// 区分请求边界错误与本地存储失败；两类错误映射到既有工具错误协议，不向浏览器泄露磁盘路径。
#[derive(Clone, Copy, Debug)]
enum MapLocalImportError {
    InvalidRequest,
    Storage,
}

/// 为 Map Local 注册有界 multipart 导入端点；较大的正文上限仅作用于该端点，不放宽普通控制请求。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    let importRoute = Router::new()
        .route(
            "/api/v1/tools/mapLocal/import",
            post(importMapLocalSelection),
        )
        .layer(DefaultBodyLimit::max(maximumImportBodyBytes));
    router.merge(importRoute)
}

/// 接收浏览器选择的单文件或目录文件集，并在完整写入后原子发布到受管映射根。
async fn importMapLocalSelection(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    multipartResult: Result<Multipart, MultipartRejection>,
) -> Result<Json<MapLocalImportResult>, LocalizedApiError> {
    let multipart = multipartResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    importMultipart(state.tools.mapLocalMappingRoot(), multipart)
        .await
        .map(Json)
        .map_err(|error| mapImportError(error).withLocale(locale))
}

/// 将 multipart 顺序流写入唯一暂存目录；任一字段或文件失败都会删除半成品，成功后才重命名为稳定目录。
async fn importMultipart(
    mappingRoot: &Path,
    mut multipart: Multipart,
) -> Result<MapLocalImportResult, MapLocalImportError> {
    let importIdentifier = Uuid::new_v4().to_string();
    let importsRoot = mappingRoot.join(importDirectoryName);
    let stagingRoot = importsRoot.join(format!(".{importIdentifier}.staging"));
    let publishedRoot = importsRoot.join(&importIdentifier);
    fs::create_dir_all(&stagingRoot)
        .await
        .map_err(|_| MapLocalImportError::Storage)?;

    let result = receiveImportFields(&stagingRoot, &mut multipart).await;
    let (directory, relativeRoot, fileCount, totalBytes) = match result {
        Ok(import) => import,
        Err(error) => {
            removeDirectoryIfPresent(&stagingRoot).await?;
            return Err(error);
        }
    };
    if (directory && relativeRoot.as_os_str().is_empty()) || (!directory && fileCount != 1) {
        removeDirectoryIfPresent(&stagingRoot).await?;
        return Err(MapLocalImportError::InvalidRequest);
    }
    if fs::rename(&stagingRoot, &publishedRoot).await.is_err() {
        removeDirectoryIfPresent(&stagingRoot).await?;
        return Err(MapLocalImportError::Storage);
    }

    let relativeImportPath = Path::new(importDirectoryName)
        .join(importIdentifier)
        .join(relativeRoot);
    Ok(MapLocalImportResult {
        localPath: pathToProtocolString(&relativeImportPath),
        fileCount,
        totalBytes,
    })
}

/// 解析 directory/path/file 三类顺序字段并流式写盘；路径必须先于对应文件，避免将未验证名称交给文件系统。
async fn receiveImportFields(
    stagingRoot: &Path,
    multipart: &mut Multipart,
) -> Result<(bool, PathBuf, usize, u64), MapLocalImportError> {
    let mut directory = None;
    let mut pendingPath = None;
    let mut directoryRoot = None;
    let mut importedPaths = HashSet::new();
    let mut fileCount = 0usize;
    let mut totalBytes = 0u64;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| MapLocalImportError::InvalidRequest)?
    {
        match field.name() {
            Some("directory") if directory.is_none() && fileCount == 0 => {
                directory = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| MapLocalImportError::InvalidRequest)?
                        .parse::<bool>()
                        .map_err(|_| MapLocalImportError::InvalidRequest)?,
                );
            }
            Some("path") if directory.is_some() && pendingPath.is_none() => {
                let path = field
                    .text()
                    .await
                    .map_err(|_| MapLocalImportError::InvalidRequest)?;
                let normalizedPath = normalizeRelativePath(&path)?;
                let duplicateKey = pathToProtocolString(&normalizedPath).to_lowercase();
                if !importedPaths.insert(duplicateKey) {
                    return Err(MapLocalImportError::InvalidRequest);
                }
                pendingPath = Some(normalizedPath);
            }
            Some("file") if pendingPath.is_some() => {
                fileCount += 1;
                if fileCount > maximumImportedFiles {
                    return Err(MapLocalImportError::InvalidRequest);
                }
                let relativePath = pendingPath
                    .take()
                    .ok_or(MapLocalImportError::InvalidRequest)?;
                updateDirectoryRoot(
                    directory.ok_or(MapLocalImportError::InvalidRequest)?,
                    &relativePath,
                    &mut directoryRoot,
                )?;
                totalBytes =
                    writeImportedFile(stagingRoot, &relativePath, field, totalBytes).await?;
            }
            _ => return Err(MapLocalImportError::InvalidRequest),
        }
    }

    if pendingPath.is_some() || fileCount == 0 {
        return Err(MapLocalImportError::InvalidRequest);
    }
    let directory = directory.ok_or(MapLocalImportError::InvalidRequest)?;
    let relativeRoot = directoryRoot.ok_or(MapLocalImportError::InvalidRequest)?;
    Ok((directory, relativeRoot, fileCount, totalBytes))
}

/// 校验浏览器提供的相对路径；只接受普通组件，绝对路径、盘符、点目录和父级跳转均在写盘前拒绝。
fn normalizeRelativePath(relativePath: &str) -> Result<PathBuf, MapLocalImportError> {
    if relativePath.is_empty() || relativePath.len() > maximumRelativePathBytes {
        return Err(MapLocalImportError::InvalidRequest);
    }
    let normalizedSeparators = relativePath.replace('\\', "/");
    let path = Path::new(&normalizedSeparators);
    let components: Vec<_> = path.components().collect();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(MapLocalImportError::InvalidRequest);
    }
    Ok(components
        .iter()
        .map(|component| component.as_os_str())
        .collect())
}

/// 保证目录导入的所有文件共享同一个浏览器根目录；单文件导入则保留原始文件名并禁止伪造子目录。
fn updateDirectoryRoot(
    directory: bool,
    relativePath: &Path,
    directoryRoot: &mut Option<PathBuf>,
) -> Result<(), MapLocalImportError> {
    let mut components = relativePath.components();
    let first = components
        .next()
        .ok_or(MapLocalImportError::InvalidRequest)?
        .as_os_str();
    if directory {
        if components.next().is_none() {
            return Err(MapLocalImportError::InvalidRequest);
        }
        let root = PathBuf::from(first);
        if directoryRoot
            .as_ref()
            .is_some_and(|current| current != &root)
        {
            return Err(MapLocalImportError::InvalidRequest);
        }
        *directoryRoot = Some(root);
    } else {
        if components.next().is_some() || directoryRoot.is_some() {
            return Err(MapLocalImportError::InvalidRequest);
        }
        *directoryRoot = Some(relativePath.to_path_buf());
    }
    Ok(())
}

/// 流式写入单个导入文件并执行单文件、总正文双重上限；超限返回请求错误，由调用方清理整个暂存目录。
async fn writeImportedFile(
    stagingRoot: &Path,
    relativePath: &Path,
    mut field: axum::extract::multipart::Field<'_>,
    initialTotalBytes: u64,
) -> Result<u64, MapLocalImportError> {
    let destination = stagingRoot.join(relativePath);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|_| MapLocalImportError::Storage)?;
    }
    let mut output = fs::File::create(destination)
        .await
        .map_err(|_| MapLocalImportError::Storage)?;
    let mut fileBytes = 0u64;
    let mut totalBytes = initialTotalBytes;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| MapLocalImportError::InvalidRequest)?
    {
        let chunkBytes =
            u64::try_from(chunk.len()).map_err(|_| MapLocalImportError::InvalidRequest)?;
        fileBytes = fileBytes
            .checked_add(chunkBytes)
            .ok_or(MapLocalImportError::InvalidRequest)?;
        totalBytes = totalBytes
            .checked_add(chunkBytes)
            .ok_or(MapLocalImportError::InvalidRequest)?;
        if fileBytes > maximumImportedFileBytes || totalBytes > maximumImportedTotalBytes {
            return Err(MapLocalImportError::InvalidRequest);
        }
        output
            .write_all(&chunk)
            .await
            .map_err(|_| MapLocalImportError::Storage)?;
    }
    output
        .flush()
        .await
        .map_err(|_| MapLocalImportError::Storage)?;
    Ok(totalBytes)
}

/// 将内部路径转换为控制协议固定的正斜杠形式，避免 Windows 分隔符进入浏览器草稿。
fn pathToProtocolString(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 清理失败导入的暂存目录；目录尚未创建或已被清理时无需覆盖原始失败语义。
async fn removeDirectoryIfPresent(path: &Path) -> Result<(), MapLocalImportError> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(MapLocalImportError::Storage),
    }
}

/// 将导入错误映射为稳定控制 API 状态；请求错误返回 400，文件系统失败返回 500。
fn mapImportError(error: MapLocalImportError) -> ApiError {
    match error {
        MapLocalImportError::InvalidRequest => ApiError::badRequest(ErrorCode::InvalidToolRequest),
        MapLocalImportError::Storage => ApiError::internal(ErrorCode::ToolOperationFailed),
    }
}
