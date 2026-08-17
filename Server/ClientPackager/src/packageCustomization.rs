//! 负责在预编译 APK 的二进制资源层修改安装身份、显示名称与应用图标。
//!
//! 运行机器不依赖 Android SDK：`resand` 重写 AXML/ARSC 字符串池，`image` 把用户图片
//! 规范化为单一 512×512 PNG，ZIP 重建继续保持 Native 16 KiB 对齐。任何模板歧义都会
//! 阻止签名，避免生成能安装但身份或图标未实际生效的 APK。

use std::io::{Cursor, Read, Write};

use image::{
    DynamicImage, GenericImageView, ImageFormat, ImageReader, Limits, imageops::FilterType,
};
use resand::{string_pool::ResStringPoolRef, table::ResTable, xmltree::XMLTree};
use zeroize::Zeroizing;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const templateApplicationId: &str = "a00000000.b00000000.c00000000.d00000000";
pub const templateApplicationName: &str = "A000000000000000";
pub(crate) const maximumIconInputBytes: usize = 1024 * 1024;
const manifestPath: &str = "AndroidManifest.xml";
const resourceTablePath: &str = "resources.arsc";
const encryptedProfilePath: &str = "assets/bootstrap/profile.bin";
const profileKeyBegin: &[u8; 16] = b"SPRKPROFKEYSLOT1";
const profileKeyEnd: &[u8; 16] = b"SPRKPROFKEYEND01";
const profileKeyBytes: usize = 32;
const iconEdgePixels: u32 = 512;
const maximumIconEdgePixels: u32 = 4096;
const maximumIconDecodeBytes: u64 = 96 * 1024 * 1024;
const nativeLibraryAlignment: u16 = 16_384;
const storedEntryAlignment: u16 = 4;

/// 收拢一次 APK 资源改写所需的可变字段；认证密文、身份和图标都只在当前重建事务中可见。
pub(crate) struct PackageCustomization<'a> {
    pub applicationId: &'a str,
    pub applicationName: &'a str,
    pub iconBytes: Option<&'a [u8]>,
    pub encryptedProfile: Option<&'a [u8]>,
    pub profileKey: Option<&'a [u8; profileKeyBytes]>,
}

/// 汇总一次 APK 重建实际命中的全部可变入口；终态校验用它证明每类资源都完整且唯一。
struct TemplateSlotCounts {
    manifest: usize,
    resourceTable: usize,
    icon: usize,
    encryptedProfile: usize,
    profileKey: usize,
}

/// 校验模板具备唯一身份、名称、图标和节点入口；失败时模板不会进入安装资源。
///
/// 运行上下文：桌面发布阶段在模板原子提交前调用。参数是完整 APK 字节；ZIP、AXML、
/// ARSC 或固定资源不符合协议时返回中文错误，不产生任何输出文件。
pub(crate) fn validateCustomizableTemplate(contents: &[u8]) -> Result<(), String> {
    let customized = PackageCustomization {
        applicationId: templateApplicationId,
        applicationName: templateApplicationName,
        iconBytes: None,
        encryptedProfile: None,
        profileKey: None,
    };
    rewriteCustomizedArchive(contents, &customized).map(|_| ())
}

/// 校验正式 APK 已写入认证密文和同一份双 ABI 随机密钥，避免签名成功掩盖静态资料装配漂移。
///
/// 运行上下文：签名并独立验签后、发布下载文件前调用。函数不返回或记录密钥；密文为空、容器头错误、
/// 任一 ABI 槽仍为零或两个槽不一致时返回静态中文错误并阻止发布。
pub(crate) fn validatePackagedProfile(contents: &[u8]) -> Result<(), String> {
    let mut archive = ZipArchive::new(Cursor::new(contents))
        .map_err(|error| format!("读取客户端成品 ZIP 失败：{error}"))?;
    let mut encryptedProfileCount = 0usize;
    let mut profileKeys = Zeroizing::new(Vec::with_capacity(2));
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取客户端成品条目失败：{error}"))?;
        let entryName = entry.name().to_owned();
        if entryName != encryptedProfilePath && !entryName.ends_with("/libroutesocks.so") {
            continue;
        }
        let mut entryBytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut entryBytes)
            .map_err(|error| format!("读取客户端成品静态资料失败：{error}"))?;
        if entryName == encryptedProfilePath {
            encryptedProfileCount += 1;
            if entryBytes.len() < 56 || !entryBytes.starts_with(b"SPRKPF01\x01\x01\0\0") {
                return Err("客户端成品认证密文容器无效".to_owned());
            }
            let plaintextLength = u32::from_be_bytes(
                entryBytes[36..40]
                    .try_into()
                    .map_err(|_| "客户端成品认证密文长度无效".to_owned())?,
            ) as usize;
            if plaintextLength == 0 || entryBytes.len() != 40 + plaintextLength + 16 {
                return Err("客户端成品认证密文长度无效".to_owned());
            }
        } else {
            profileKeys.push(readProfileKeySlot(&entryBytes)?);
        }
    }
    if encryptedProfileCount != 1 || profileKeys.len() != 2 {
        return Err("客户端成品认证资料入口数量无效".to_owned());
    }
    if profileKeys[0].iter().all(|byte| *byte == 0) || profileKeys[0] != profileKeys[1] {
        return Err("客户端成品双 ABI 静态资料密钥不一致".to_owned());
    }
    Ok(())
}

/// 重建 APK 并写入可变身份、软件名、图标和节点；输出仍是未签名 ZIP 字节。
///
/// 运行上下文：独立打包器在阻塞线程内调用；`customization` 已通过上层业务校验。
/// 任一目标槽位缺失、重复、图片无效或 ZIP 写入失败时返回错误，调用方不得继续签名。
pub(crate) fn rewriteCustomizedArchive(
    contents: &[u8],
    customization: &PackageCustomization<'_>,
) -> Result<Vec<u8>, String> {
    let normalizedIcon = customization.iconBytes.map(normalizeIcon).transpose()?;
    let mut sourceArchive = ZipArchive::new(Cursor::new(contents))
        .map_err(|error| format!("读取客户端模板 ZIP 失败：{error}"))?;
    let mut destinationArchive = ZipWriter::new(Cursor::new(Vec::with_capacity(contents.len())));
    let mut manifestCount = 0usize;
    let mut resourceTableCount = 0usize;
    let mut iconCount = 0usize;
    let mut profileCount = 0usize;
    let mut profileKeyCount = 0usize;

    for index in 0..sourceArchive.len() {
        let mut entry = sourceArchive
            .by_index(index)
            .map_err(|error| format!("读取客户端模板条目失败：{error}"))?;
        let entryName = entry.name().to_owned();
        if entry.is_dir() {
            destinationArchive
                .add_directory(entryName, SimpleFileOptions::default())
                .map_err(|error| format!("写入客户端模板目录失败：{error}"))?;
            continue;
        }
        let compression = supportedCompression(entry.compression())?;
        let mut entryBytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut entryBytes)
            .map_err(|error| format!("读取客户端模板条目正文失败：{error}"))?;
        if entryName == manifestPath {
            entryBytes = rewriteManifest(&entryBytes, customization.applicationId)?;
            manifestCount += 1;
        } else if entryName == resourceTablePath {
            entryBytes = rewriteResourceTable(
                &entryBytes,
                customization.applicationId,
                customization.applicationName,
            )?;
            resourceTableCount += 1;
        } else if isTemplateIcon(&entryName, &entryBytes) {
            iconCount += 1;
            if let Some(icon) = normalizedIcon.as_ref() {
                entryBytes.clone_from(icon);
            }
        }
        if entryName == encryptedProfilePath {
            if !entryBytes.is_empty() {
                return Err("客户端模板静态资料占位必须为空".to_owned());
            }
            if let Some(encryptedProfile) = customization.encryptedProfile {
                entryBytes = encryptedProfile.to_vec();
            }
            profileCount += 1;
        }
        if entryName.ends_with("/libroutesocks.so") {
            profileKeyCount += patchProfileKeySlot(&mut entryBytes, customization.profileKey)?;
        }
        let alignment = if compression == CompressionMethod::Stored && entryName.ends_with(".so") {
            nativeLibraryAlignment
        } else if compression == CompressionMethod::Stored {
            storedEntryAlignment
        } else {
            1
        };
        destinationArchive
            .start_file(
                entryName,
                SimpleFileOptions::default()
                    .compression_method(compression)
                    .with_alignment(alignment),
            )
            .map_err(|error| format!("创建客户端模板条目失败：{error}"))?;
        destinationArchive
            .write_all(&entryBytes)
            .map_err(|error| format!("写入客户端模板条目失败：{error}"))?;
    }
    validateSlotCounts(TemplateSlotCounts {
        manifest: manifestCount,
        resourceTable: resourceTableCount,
        icon: iconCount,
        encryptedProfile: profileCount,
        profileKey: profileKeyCount,
    })?;
    destinationArchive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("完成客户端模板 ZIP 失败：{error}"))
}

/// 校验并改写 Native 每包密钥槽；准备模板时要求零槽，正式装配时在两个 ABI 中写入同一随机密钥。
fn patchProfileKeySlot(
    library: &mut [u8],
    profileKey: Option<&[u8; profileKeyBytes]>,
) -> Result<usize, String> {
    let slotBytes = profileKeyBegin.len() + profileKeyBytes + profileKeyEnd.len();
    let offsets = library
        .windows(slotBytes)
        .enumerate()
        .filter_map(|(offset, window)| {
            (&window[..profileKeyBegin.len()] == profileKeyBegin
                && &window[profileKeyBegin.len() + profileKeyBytes..] == profileKeyEnd)
                .then_some(offset)
        })
        .collect::<Vec<_>>();
    if offsets.len() != 1 {
        return Err(format!(
            "客户端 Native 静态资料密钥槽必须唯一，实际为 {}",
            offsets.len()
        ));
    }
    let keyOffset = offsets[0] + profileKeyBegin.len();
    let keySlot = &mut library[keyOffset..keyOffset + profileKeyBytes];
    match profileKey {
        Some(key) => keySlot.copy_from_slice(key),
        None if keySlot.iter().all(|byte| *byte == 0) => {}
        None => return Err("客户端模板 Native 静态资料密钥槽必须为零".to_owned()),
    }
    Ok(1)
}

/// 从单个 Native 核心读取唯一密钥槽；该辅助函数只用于成品一致性校验，不向调用方暴露密钥。
fn readProfileKeySlot(library: &[u8]) -> Result<[u8; profileKeyBytes], String> {
    let slotBytes = profileKeyBegin.len() + profileKeyBytes + profileKeyEnd.len();
    let offsets = library
        .windows(slotBytes)
        .enumerate()
        .filter_map(|(offset, window)| {
            (&window[..profileKeyBegin.len()] == profileKeyBegin
                && &window[profileKeyBegin.len() + profileKeyBytes..] == profileKeyEnd)
                .then_some(offset)
        })
        .collect::<Vec<_>>();
    if offsets.len() != 1 {
        return Err("客户端成品 Native 静态资料密钥槽必须唯一".to_owned());
    }
    let keyOffset = offsets[0] + profileKeyBegin.len();
    let mut key = [0_u8; profileKeyBytes];
    key.copy_from_slice(&library[keyOffset..keyOffset + profileKeyBytes]);
    Ok(key)
}

/// 重写 AndroidManifest 字符串池中全部包名前缀，覆盖包名、动态权限和 provider authority。
fn rewriteManifest(contents: &[u8], applicationId: &str) -> Result<Vec<u8>, String> {
    let mut tree = XMLTree::try_from(contents)
        .map_err(|error| format!("解析客户端二进制清单失败：{error}"))?;
    let replacements = tree
        .string_pool
        .string_pool
        .get_strings()
        .enumerate()
        .filter(|(_, value)| value.contains(templateApplicationId))
        .map(|(index, value)| {
            (
                ResStringPoolRef {
                    index: index as u32,
                },
                value.replace(templateApplicationId, applicationId),
            )
        })
        .collect::<Vec<_>>();
    if replacements.is_empty() {
        return Err("客户端二进制清单缺少包名槽位".to_owned());
    }
    for (reference, value) in replacements {
        tree.string_pool.write(value, reference);
    }
    Vec::<u8>::try_from(tree).map_err(|error| format!("写入客户端二进制清单失败：{error}"))
}

/// 重写资源表包名和唯一软件名字符串；其它资源索引保持原值，避免重新编译资源。
fn rewriteResourceTable(
    contents: &[u8],
    applicationId: &str,
    applicationName: &str,
) -> Result<Vec<u8>, String> {
    let mut table = ResTable::read_all(&mut Cursor::new(contents))
        .map_err(|error| format!("解析客户端资源表失败：{error}"))?;
    let package = table
        .packages
        .get_mut(0)
        .ok_or_else(|| "客户端资源表缺少主包".to_owned())?;
    if package.name != templateApplicationId {
        return Err("客户端资源表包名不是发布占位值".to_owned());
    }
    package.name = applicationId.to_owned();
    let nameReferences = table
        .string_pool
        .string_pool
        .get_strings()
        .enumerate()
        .filter_map(|(index, value)| {
            (value == templateApplicationName).then_some(ResStringPoolRef {
                index: index as u32,
            })
        })
        .collect::<Vec<_>>();
    if nameReferences.len() != 1 {
        return Err(format!(
            "客户端资源表软件名槽位必须唯一，实际为 {}",
            nameReferences.len()
        ));
    }
    table
        .string_pool
        .write(applicationName.to_owned(), nameReferences[0]);
    let mut output = Cursor::new(Vec::with_capacity(contents.len()));
    table
        .write_all(&mut output)
        .map_err(|error| format!("写入客户端资源表失败：{error}"))?;
    Ok(output.into_inner())
}

/// 将常见位图解码、居中裁成正方形并缩放为模板唯一图标尺寸。
fn normalizeIcon(contents: &[u8]) -> Result<Vec<u8>, String> {
    if contents.is_empty() || contents.len() > maximumIconInputBytes {
        return Err(format!(
            "自定义图标必须位于 1..={maximumIconInputBytes} 字节"
        ));
    }
    // 先只读取图片头部尺寸，再给完整解码设置严格尺寸和内存预算，避免小型压缩炸弹在拒绝前触发巨量分配。
    let metadataReader = customIconReader(contents)?;
    let (width, height) = metadataReader
        .into_dimensions()
        .map_err(|_| "自定义图标必须是有效 PNG、JPEG 或 WebP 图片".to_owned())?;
    if width == 0 || height == 0 || width > maximumIconEdgePixels || height > maximumIconEdgePixels
    {
        return Err(format!(
            "自定义图标边长必须位于 1..={maximumIconEdgePixels} 像素"
        ));
    }
    let mut decodeReader = customIconReader(contents)?;
    let mut decodeLimits = Limits::default();
    decodeLimits.max_image_width = Some(maximumIconEdgePixels);
    decodeLimits.max_image_height = Some(maximumIconEdgePixels);
    decodeLimits.max_alloc = Some(maximumIconDecodeBytes);
    decodeReader.limits(decodeLimits);
    let decoded = decodeReader
        .decode()
        .map_err(|_| "自定义图标解码失败或超过内存预算".to_owned())?;
    let edge = width.min(height);
    let cropped = decoded.crop_imm((width - edge) / 2, (height - edge) / 2, edge, edge);
    let resized = cropped.resize_exact(iconEdgePixels, iconEdgePixels, FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(resized.to_rgba8())
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|error| format!("编码自定义图标失败：{error}"))?;
    Ok(output.into_inner())
}

/// 创建只允许 PNG、JPEG 与 WebP 的内存图片读取器；格式未知或不在白名单时返回固定中文错误。
fn customIconReader(contents: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    let reader = ImageReader::new(Cursor::new(contents))
        .with_guessed_format()
        .map_err(|_| "无法识别自定义图标格式".to_owned())?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        return Err("自定义图标必须是 PNG、JPEG 或 WebP 图片".to_owned());
    }
    Ok(reader)
}

/// 识别模板中唯一的 512×512 PNG 图标条目。
///
/// 发布构建可能由资源优化器重写 ZIP 条目名称，因此不能依赖 `app_icon.png`
/// 这一文件名；模板校验会同时要求图标数量恰好为一个，避免把其他图片误当作图标。
fn isTemplateIcon(entryName: &str, contents: &[u8]) -> bool {
    if !entryName.starts_with("res/") || !contents.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    image::load_from_memory_with_format(contents, ImageFormat::Png)
        .map(|image| image.dimensions() == (iconEdgePixels, iconEdgePixels))
        .unwrap_or(false)
}

/// 限定 ZIP 只使用运行时已验证的 Stored/Deflated 算法。
fn supportedCompression(compression: CompressionMethod) -> Result<CompressionMethod, String> {
    match compression {
        CompressionMethod::Stored | CompressionMethod::Deflated => Ok(compression),
        method => Err(format!("客户端模板包含不支持的 ZIP 压缩算法：{method:?}")),
    }
}

/// 校验四类模板入口均唯一，防止只改到部分资源或把无关图片当作应用图标。
fn validateSlotCounts(counts: TemplateSlotCounts) -> Result<(), String> {
    if counts.manifest != 1
        || counts.resourceTable != 1
        || counts.icon != 1
        || counts.encryptedProfile != 1
        || counts.profileKey != 2
    {
        return Err(format!(
            "客户端模板可变入口数量无效：清单 {}，资源表 {}，图标 {}，密文资料 {}，Native 密钥槽 {}",
            counts.manifest,
            counts.resourceTable,
            counts.icon,
            counts.encryptedProfile,
            counts.profileKey,
        ));
    }
    Ok(())
}
