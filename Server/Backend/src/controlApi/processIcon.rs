//! 从 Windows 可执行文件提取系统关联图标，并编码为浏览器可直接显示的 PNG。
//!
//! 图标只用于进程选择器展示，不进入持久化配置。Shell 返回的图标在内存 DIB 中绘制后立即释放，
//! 避免刷新进程列表时泄漏桌面句柄。

use std::{io::Cursor, path::Path};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::{
    Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ, SelectObject,
        },
        Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        UI::{
            Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW},
            WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON},
        },
    },
    core::PCWSTR,
};

const iconPixelSize: u32 = 48;

/// 提取可执行文件的系统图标并返回 PNG；路径无图标或平台不支持时返回 None，界面显示统一占位图。
#[cfg(windows)]
pub(super) fn extractProcessIcon(executablePath: &Path) -> Option<Vec<u8>> {
    let widePath = executablePath
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut fileInfo = SHFILEINFOW::default();
    // Shell 返回的 HICON 由调用方所有；无论绘制是否成功都在本函数内释放。
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(widePath.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut fileInfo),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || fileInfo.hIcon.0.is_null() {
        return None;
    }

    let encodedIcon = renderIcon(fileInfo.hIcon);
    let _ = unsafe { DestroyIcon(fileInfo.hIcon) };
    encodedIcon
}

/// 在顶向下 32 位 DIB 中绘制 HICON；任一 GDI 或编码步骤失败时完整释放资源并返回 None。
#[cfg(windows)]
fn renderIcon(icon: HICON) -> Option<Vec<u8>> {
    let deviceContext = unsafe { CreateCompatibleDC(None) };
    if deviceContext.0.is_null() {
        return None;
    }

    let bitmapInfo = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: iconPixelSize as i32,
            biHeight: -(iconPixelSize as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: iconPixelSize * iconPixelSize * 4,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut pixelAddress = std::ptr::null_mut();
    let bitmapResult = unsafe {
        CreateDIBSection(
            Some(deviceContext),
            &bitmapInfo,
            DIB_RGB_COLORS,
            &mut pixelAddress,
            None,
            0,
        )
    };
    let bitmap = match bitmapResult {
        Ok(bitmap) => bitmap,
        Err(_) => {
            let _ = unsafe { DeleteDC(deviceContext) };
            return None;
        }
    };
    let previousObject = unsafe { SelectObject(deviceContext, HGDIOBJ(bitmap.0)) };
    let drawResult = unsafe {
        DrawIconEx(
            deviceContext,
            0,
            0,
            icon,
            iconPixelSize as i32,
            iconPixelSize as i32,
            0,
            None,
            DI_NORMAL,
        )
    };

    let encodedIcon = if drawResult.is_ok() && !pixelAddress.is_null() {
        let byteCount = (iconPixelSize * iconPixelSize * 4) as usize;
        // DIB 生命周期覆盖此切片，且 SelectObject 恢复前没有其它线程能够访问该私有 DC。
        let bgraPixels =
            unsafe { std::slice::from_raw_parts(pixelAddress.cast::<u8>(), byteCount) };
        encodePng(bgraPixels)
    } else {
        None
    };
    unsafe {
        SelectObject(deviceContext, previousObject);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(deviceContext);
    }
    encodedIcon
}

/// 将 GDI 的预乘 BGRA 像素转换为非预乘 RGBA PNG；旧图标没有 alpha 时保留其不透明颜色。
fn encodePng(bgraPixels: &[u8]) -> Option<Vec<u8>> {
    let hasAlpha = bgraPixels.chunks_exact(4).any(|pixel| pixel[3] != 0);
    let mut rgbaPixels = Vec::with_capacity(bgraPixels.len());
    for pixel in bgraPixels.chunks_exact(4) {
        let alpha = if hasAlpha { pixel[3] } else { 255 };
        let unpremultiply = |component: u8| -> u8 {
            if alpha == 0 || alpha == 255 {
                component
            } else {
                ((u16::from(component) * 255) / u16::from(alpha)).min(255) as u8
            }
        };
        rgbaPixels.extend_from_slice(&[
            unpremultiply(pixel[2]),
            unpremultiply(pixel[1]),
            unpremultiply(pixel[0]),
            alpha,
        ]);
    }

    let mut pngBytes = Vec::new();
    {
        let mut encoder =
            png::Encoder::new(Cursor::new(&mut pngBytes), iconPixelSize, iconPixelSize);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .ok()?
            .write_image_data(&rgbaPixels)
            .ok()?;
    }
    Some(pngBytes)
}

/// 非 Windows 构建不提供可执行文件图标；保持同一调用契约以便跨平台检查控制面。
#[cfg(not(windows))]
pub(super) fn extractProcessIcon(_executablePath: &Path) -> Option<Vec<u8>> {
    None
}
