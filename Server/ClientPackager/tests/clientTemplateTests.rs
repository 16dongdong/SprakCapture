#![allow(non_snake_case, non_upper_case_globals)]

use std::{
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};

use apksig::Apk;
use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use client_packager::{
    ClientTemplateRequest, packageClientTemplate, prepareClientTemplate, templateApplicationId,
    templateProfilePayload,
};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgba};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const generatedApplicationId: &str = "q01234567.r89abcdef.s01234567.t89abcdef";

/// Client 编译资产必须与独立打包器槽位逐字节一致，避免发布阶段才发现模板协议漂移。
#[test]
fn clientAssetMatchesPackagerTemplatePayload() {
    assert_eq!(
        templateProfilePayload,
        include_bytes!("../../../Client/app/src/main/assets/bootstrap/profile.bin")
    );
}

/// 创建只包含真实 AAPT2 二进制资源和最小 Native 槽的 APK；打包器因此在无 Android SDK 的测试机上仍能验证重写协议。
fn createCompiledSource(path: &Path) {
    createCompiledSourceWithKey(path, 0);
}

/// 创建可选择密钥槽初始值的真实资源模板；非零值仅用于证明正式打包会再次拒绝被污染模板。
fn createCompiledSourceWithKey(path: &Path, profileKeyByte: u8) {
    let file = fs::File::create(path).expect("应能创建测试模板");
    let mut archive = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    archive
        .start_file("AndroidManifest.xml", deflated)
        .expect("应能创建清单条目");
    archive
        .write_all(include_bytes!("fixtures/AndroidManifest.xml"))
        .expect("应能写入真实二进制清单");
    archive
        .start_file("resources.arsc", stored)
        .expect("应能创建资源条目");
    archive
        .write_all(include_bytes!("fixtures/resources.arsc"))
        .expect("应能写入真实资源表");
    archive
        .start_file("res/drawable-nodpi-v4/app_icon.png", stored)
        .expect("应能创建图标条目");
    archive
        .write_all(include_bytes!("fixtures/app_icon.png"))
        .expect("应能写入默认图标");
    archive
        .start_file("assets/bootstrap/profile.bin", stored)
        .expect("应能创建节点资产条目");
    archive
        .write_all(templateProfilePayload)
        .expect("应能写入节点槽位");
    for libraryPath in [
        "lib/arm64-v8a/libroutesocks.so",
        "lib/armeabi-v7a/libroutesocks.so",
    ] {
        archive
            .start_file(libraryPath, stored)
            .expect("应能创建统一 Native 条目");
        let mut library = b"native-SPRKPROFKEYSLOT1".to_vec();
        library.extend(std::iter::repeat_n(profileKeyByte, 32));
        library.extend_from_slice(b"SPRKPROFKEYEND01");
        archive
            .write_all(&library)
            .expect("应能写入统一 Native 条目");
    }
    archive.finish().expect("应能完成测试模板");
}

/// 正式打包必须重新校验模板零槽，不能依赖历史 prepare 结果覆盖被替换模板中的未知旧密钥。
#[test]
fn rejectsContaminatedTemplateProfileKey() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let templatePath = directory.path().join("contaminated.apk");
    let destinationPath = directory.path().join("invalid.apk");
    createCompiledSourceWithKey(&templatePath, 0x5A);
    let result = packageClientTemplate(&ClientTemplateRequest {
        templatePath,
        destinationPath: destinationPath.clone(),
        signingDirectory: directory.path().join("signing"),
        applicationId: generatedApplicationId.to_owned(),
        applicationName: "5A17C290E4B86D31".to_owned(),
        nodeHost: "192.0.2.10".to_owned(),
        nodePort: 54_321,
        username: "client-user".to_owned(),
        password: "client-password".to_owned(),
        rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
        iconBytes: None,
    });
    let error = match result {
        Ok(_) => panic!("非零模板密钥槽必须阻止正式打包"),
        Err(error) => error,
    };
    assert!(error.contains("密钥槽必须为零"), "{error}");
    assert!(!destinationPath.exists());
}

/// 验证独立打包器重写身份、加密静态资料、生成安装签名并复用持久签名身份；任一环节漂移都会失败。
#[test]
fn packagesAndSignsPrecompiledTemplate() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    let firstOutput = directory.path().join("first.apk");
    let secondOutput = directory.path().join("second.apk");
    let signingDirectory = directory.path().join("signing");
    createCompiledSource(&sourcePath);
    prepareClientTemplate(&sourcePath, &templatePath).expect("应能规范化预编译模板");
    let templateFile = fs::File::open(&templatePath).expect("应能读取规范化模板");
    let mut templateArchive = ZipArchive::new(templateFile).expect("规范化模板应是 ZIP");
    let manifest = templateArchive
        .by_name("AndroidManifest.xml")
        .expect("模板应包含清单");
    assert_eq!(manifest.compression(), CompressionMethod::Stored);
    drop(manifest);
    let nativeLibrary = templateArchive
        .by_name("lib/arm64-v8a/libroutesocks.so")
        .expect("模板应包含 Native 核心");
    assert_eq!(nativeLibrary.data_start() % 16_384, 0);
    drop(nativeLibrary);

    for destinationPath in [&firstOutput, &secondOutput] {
        let result = packageClientTemplate(&ClientTemplateRequest {
            templatePath: templatePath.clone(),
            destinationPath: destinationPath.clone(),
            signingDirectory: signingDirectory.clone(),
            applicationId: generatedApplicationId.to_owned(),
            applicationName: "5A17C290E4B86D31".to_owned(),
            nodeHost: "192.168.10.24".to_owned(),
            nodePort: 54_321,
            username: "client-user".to_owned(),
            password: "client-password".to_owned(),
            rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt"
                .to_owned(),
            iconBytes: None,
        })
        .expect("预编译模板应能装配并签名");
        let bytes = fs::read(destinationPath).expect("应能读取打包产物");
        assert_eq!(result.bytes, bytes);
        let generatedPackageBytes = generatedApplicationId
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!(
            bytes
                .windows(generatedPackageBytes.len())
                .any(|value| value == generatedPackageBytes)
        );
        let placeholder = templateApplicationId
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!(
            !bytes
                .windows(placeholder.len())
                .any(|value| value == placeholder)
        );
        assertArchiveDoesNotContainSecrets(
            &bytes,
            &[
                "192.168.10.24".to_owned(),
                "54321".to_owned(),
                "192.168.10.24:54321".to_owned(),
                "client-user".to_owned(),
                "client-password".to_owned(),
                base64Standard.encode("192.168.10.24"),
                base64Standard.encode("54321"),
                base64Standard.encode("192.168.10.24:54321"),
                base64Standard.encode("client-user"),
                base64Standard.encode("client-password"),
                "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
                base64Standard
                    .encode("http://client-rules.internal.invalid:19090/api/v1/client/routing.txt"),
            ],
        );
        assertPackagedProfileIsAuthenticated(&bytes);
        assert!(bytes.windows(16).any(|value| value == b"5A17C290E4B86D31"));
        Apk::new(destinationPath.clone())
            .expect("产物应包含 APK 签名块")
            .verify()
            .expect("产物签名应有效");
    }
    assert!(signingDirectory.join("signingIdentity.json").is_file());
    let transientFiles = fs::read_dir(directory.path())
        .expect("应能检查客户端装配目录")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            (name.starts_with('.') && name.ends_with(".apk"))
                || name.starts_with(".signingIdentity-")
        })
        .collect::<Vec<_>>();
    assert!(
        transientFiles.is_empty(),
        "成功装配后不得残留含凭据或私钥的暂存文件：{transientFiles:?}"
    );
}

/// 短数字凭据可能自然出现在 Android 资源或二进制常量中；打包仍须成功，并以 Base64 与完整资料结构验证未泄露。
#[test]
fn packagesShortNumericCredentialsWithoutStaticScanCollision() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    let destinationPath = directory.path().join("shortCredentials.apk");
    createCompiledSource(&sourcePath);
    prepareClientTemplate(&sourcePath, &templatePath).expect("应能规范化预编译模板");

    packageClientTemplate(&ClientTemplateRequest {
        templatePath,
        destinationPath: destinationPath.clone(),
        signingDirectory: directory.path().join("signing"),
        applicationId: "short.credential.client".to_owned(),
        applicationName: "ShortCredentials".to_owned(),
        nodeHost: "192.0.2.10".to_owned(),
        nodePort: 54_321,
        username: "123456".to_owned(),
        password: "654321".to_owned(),
        rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
        iconBytes: None,
    })
    .expect("短数字凭据不应因资源中的普通数字片段被误判为静态泄露");

    let packageBytes = fs::read(&destinationPath).expect("应能读取短凭据客户端成品");
    assertArchiveDoesNotContainSecrets(
        &packageBytes,
        &[
            base64Standard.encode("123456"),
            base64Standard.encode("654321"),
            "\"username\":\"123456\"".to_owned(),
            "\"password\":\"654321\"".to_owned(),
        ],
    );
    assertPackagedProfileIsAuthenticated(&packageBytes);
    Apk::new(destinationPath)
        .expect("短凭据客户端应包含 APK 签名块")
        .verify()
        .expect("短凭据客户端签名应有效");
}

/// 自定义图标必须在无 Android SDK 的打包阶段完成裁剪与规范化，并实际覆盖唯一的发布图标资源。
#[test]
fn packagesCustomizedApplicationIcon() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    let destinationPath = directory.path().join("customIcon.apk");
    createCompiledSource(&sourcePath);
    prepareClientTemplate(&sourcePath, &templatePath).expect("应能规范化预编译模板");
    packageClientTemplate(&ClientTemplateRequest {
        templatePath,
        destinationPath: destinationPath.clone(),
        signingDirectory: directory.path().join("signing"),
        applicationId: "custom.icon.client".to_owned(),
        applicationName: "CustomIcon".to_owned(),
        nodeHost: "192.0.2.10".to_owned(),
        nodePort: 54_321,
        username: "icon-user".to_owned(),
        password: "icon-password".to_owned(),
        rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
        iconBytes: Some(createCustomIcon()),
    })
    .expect("自定义图标应能装配并签名");

    let packageFile = fs::File::open(&destinationPath).expect("应能读取自定义客户端");
    let mut packageArchive = ZipArchive::new(packageFile).expect("自定义客户端必须是 APK ZIP");
    let mut iconBytes = Vec::new();
    packageArchive
        .by_name("res/drawable-nodpi-v4/app_icon.png")
        .expect("自定义客户端必须保留唯一图标入口")
        .read_to_end(&mut iconBytes)
        .expect("应能读取自定义图标");
    let icon = image::load_from_memory_with_format(&iconBytes, ImageFormat::Png)
        .expect("装配后图标必须是有效 PNG");
    assert_eq!(icon.dimensions(), (512, 512));
    assert!(icon.get_pixel(128, 256).0[0] > 200);
    assert!(icon.get_pixel(384, 256).0[2] > 200);
}

/// 生成宽屏红蓝测试图；安全取样点可以证明居中裁切与缩放都已真正生效。
fn createCustomIcon() -> Vec<u8> {
    let image = ImageBuffer::from_fn(32, 16, |x, _| {
        if x < 16 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .expect("应能生成自定义图标夹具");
    output.into_inner()
}

/// 校验成品 profile 容器完整，且双 ABI Native 槽持有同一份非零随机密钥；函数不输出密钥内容。
fn assertPackagedProfileIsAuthenticated(apkBytes: &[u8]) {
    let mut archive = ZipArchive::new(std::io::Cursor::new(apkBytes)).expect("产物必须是 APK ZIP");
    let mut profile = Vec::new();
    archive
        .by_name("assets/bootstrap/profile.bin")
        .expect("产物必须包含认证密文")
        .read_to_end(&mut profile)
        .expect("应能读取认证密文");
    assert!(profile.starts_with(b"SPRKPF01\x01\x01\0\0"));
    let plaintextLength = u32::from_be_bytes(profile[36..40].try_into().expect("长度字段完整"));
    assert_eq!(profile.len(), 40 + plaintextLength as usize + 16);
    let mut keys = Vec::new();
    for libraryPath in [
        "lib/arm64-v8a/libroutesocks.so",
        "lib/armeabi-v7a/libroutesocks.so",
    ] {
        let mut library = Vec::new();
        archive
            .by_name(libraryPath)
            .expect("产物必须包含双 ABI Native 核心")
            .read_to_end(&mut library)
            .expect("应能读取 Native 核心");
        let marker = b"SPRKPROFKEYSLOT1";
        let offsets = library
            .windows(marker.len())
            .enumerate()
            .filter_map(|(offset, window)| (window == marker).then_some(offset))
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), 1, "每个 ABI 必须只有一个密钥槽");
        keys.push(library[offsets[0] + marker.len()..offsets[0] + marker.len() + 32].to_vec());
    }
    assert!(keys[0].iter().any(|byte| *byte != 0));
    assert_eq!(keys[0], keys[1]);
}

/// 同时扫描完整 APK 和所有解压 ZIP 条目，防止压缩资源中的明文绕过整包字节搜索。
fn assertArchiveDoesNotContainSecrets(apkBytes: &[u8], secrets: &[String]) {
    for secret in secrets {
        assert!(
            !apkBytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "完整 APK 不得包含静态连接资料明文"
        );
    }
    let mut archive = ZipArchive::new(std::io::Cursor::new(apkBytes)).expect("产物必须是 APK ZIP");
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("应能读取 APK 条目");
        let mut contents = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut contents).expect("应能解压 APK 条目");
        for secret in secrets {
            assert!(
                !contents
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "APK 解压条目不得包含静态连接资料明文"
            );
        }
    }
}

/// 验证模板准备拒绝额外动态库；发布包必须只有统一核心，不能把旧 HEV 或第三方 SO 带入客户端。
#[test]
fn rejectsTemplateContainingAdditionalNativeLibrary() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let invalidPath = directory.path().join("invalid.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    createCompiledSource(&sourcePath);

    let sourceFile = fs::File::open(&sourcePath).expect("应能读取有效模板");
    let mut sourceArchive = ZipArchive::new(sourceFile).expect("有效模板应是 ZIP");
    let invalidFile = fs::File::create(&invalidPath).expect("应能创建非法模板");
    let mut invalidArchive = ZipWriter::new(invalidFile);
    for index in 0..sourceArchive.len() {
        let entry = sourceArchive.by_index(index).expect("应能读取模板条目");
        invalidArchive
            .raw_copy_file(entry)
            .expect("应能复制模板条目");
    }
    invalidArchive
        .start_file(
            "lib/arm64-v8a/libhev-socks5-tunnel.so",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .expect("应能写入额外 Native 条目");
    invalidArchive
        .write_all(b"legacy")
        .expect("应能写入额外 Native 内容");
    invalidArchive.finish().expect("应能完成非法模板");

    let error = prepareClientTemplate(&invalidPath, &templatePath)
        .err()
        .expect("额外 Native 条目必须阻止模板发布");
    assert!(error.contains("必须仅包含双 ABI 统一 Native 核心"));
    assert!(!templatePath.exists());
}

/// 验证独立打包入口拒绝域名或非规范 IP，避免签名后才由 Android 静态资料解析器判定产物无效。
#[test]
fn rejectsNonCanonicalNodeHost() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    let destinationPath = directory.path().join("invalid.apk");
    createCompiledSource(&sourcePath);
    prepareClientTemplate(&sourcePath, &templatePath).expect("应能规范化预编译模板");
    let error = packageClientTemplate(&ClientTemplateRequest {
        templatePath,
        destinationPath: destinationPath.clone(),
        signingDirectory: directory.path().join("signing"),
        applicationId: generatedApplicationId.to_owned(),
        applicationName: "5A17C290E4B86D31".to_owned(),
        nodeHost: "node.example.com".to_owned(),
        nodePort: 1080,
        username: "client-user".to_owned(),
        password: "client-password".to_owned(),
        rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
        iconBytes: None,
    })
    .err()
    .expect("非 IP 节点必须失败");
    assert!(error.contains("必须是规范 IP 字面量"));
    assert!(!destinationPath.exists());
}

/// 模板装配必须拒绝空密码，保证生成包中的 SOCKS 握手和规则下载始终共享一组完整、可重复使用的凭据。
#[test]
fn rejectsEmptyEmbeddedPassword() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let sourcePath = directory.path().join("compiled.apk");
    let templatePath = directory.path().join("clientTemplate.apk");
    let destinationPath = directory.path().join("invalid.apk");
    createCompiledSource(&sourcePath);
    prepareClientTemplate(&sourcePath, &templatePath).expect("应能规范化预编译模板");

    let error = packageClientTemplate(&ClientTemplateRequest {
        templatePath,
        destinationPath: destinationPath.clone(),
        signingDirectory: directory.path().join("signing"),
        applicationId: generatedApplicationId.to_owned(),
        applicationName: "5A17C290E4B86D31".to_owned(),
        nodeHost: "192.168.10.24".to_owned(),
        nodePort: 1080,
        username: "fixed-user".to_owned(),
        password: String::new(),
        rulesUrl: "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt".to_owned(),
        iconBytes: None,
    })
    .err()
    .expect("空密码必须阻止模板装配");
    assert!(error.contains("客户端密码长度必须位于 1..=255 个 UTF-8 字节"));
    assert!(!destinationPath.exists());
}

/// 验证独立打包入口与 Android 使用同一规则 URL 接受集；畸形 authority、端口和附加 URI 字段必须在签名前拒绝。
#[test]
fn rejectsMalformedRulesUrl() {
    let directory = TempDir::new().expect("应能创建测试目录");
    let templatePath = directory.path().join("template.apk");
    fs::write(&templatePath, b"placeholder").expect("应能创建请求校验占位模板");
    for rulesUrl in [
        "http://:19090/api/v1/client/routing.txt",
        "http://client-rules.internal.invalid:0/api/v1/client/routing.txt",
        "http://client-rules.internal.invalid:99999/api/v1/client/routing.txt",
        "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt?x=1",
        "http://user@client-rules.internal.invalid:19090/api/v1/client/routing.txt",
    ] {
        let error = packageClientTemplate(&ClientTemplateRequest {
            templatePath: templatePath.clone(),
            destinationPath: directory.path().join("invalid.apk"),
            signingDirectory: directory.path().join("signing"),
            applicationId: generatedApplicationId.to_owned(),
            applicationName: "5A17C290E4B86D31".to_owned(),
            nodeHost: "192.0.2.10".to_owned(),
            nodePort: 1080,
            username: "client-user".to_owned(),
            password: "client-password".to_owned(),
            rulesUrl: rulesUrl.to_owned(),
            iconBytes: None,
        })
        .err()
        .expect("畸形规则地址必须在读取模板前失败");
        assert!(error.contains("客户端规则地址"));
    }
}
