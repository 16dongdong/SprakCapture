import org.jetbrains.kotlin.gradle.dsl.JvmTarget

val packagedApplicationId = providers.gradleProperty("clientApplicationId")
    .orElse("a00000000.b00000000.c00000000.d00000000")
    .get()
    .also { applicationId ->
        require(applicationId.matches(Regex("^[a-z][a-z0-9]*(?:\\.[a-z][a-z0-9]*){2,}$"))) {
            "clientApplicationId 必须是至少三段的小写 Android 包名"
        }
    }
val packagedBuildDirectory = providers.gradleProperty("clientBuildDirectory")
val signingStoreFile = providers.environmentVariable("CLIENT_SIGNING_STORE_FILE")
val signingStorePassword = providers.environmentVariable("CLIENT_SIGNING_STORE_PASSWORD")
val signingKeyAlias = providers.environmentVariable("CLIENT_SIGNING_KEY_ALIAS")
val signingKeyPassword = providers.environmentVariable("CLIENT_SIGNING_KEY_PASSWORD")

if (packagedBuildDirectory.isPresent) {
    // 桌面发布阶段把预编译模板放入任务专属目录，运行时只分发 APK，不携带源码或 Gradle 缓存。
    layout.buildDirectory.set(file(packagedBuildDirectory.get()))
}

plugins {
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.kotlinAndroid)
    alias(libs.plugins.kotlinCompose)
}

android {
    namespace = "app.proxy.client"
    compileSdk = 36
    ndkVersion = "25.1.8937393"

    defaultConfig {
        applicationId = packagedApplicationId
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"

        externalNativeBuild {
            ndkBuild {
                // 模板构建一次性产出双 ABI libroutesocks；桌面打包器只注入固定槽位，不携带 NDK 或源码。
                abiFilters += listOf("arm64-v8a", "armeabi-v7a")
            }
        }
    }

    val packagedReleaseSigning = if (
        signingStoreFile.isPresent &&
        signingStorePassword.isPresent &&
        signingKeyAlias.isPresent &&
        signingKeyPassword.isPresent
    ) {
        signingConfigs.create("packagedRelease") {
            // 手工或 CI 直签时只从环境读取材料；桌面模板构建不提供这些变量并保持未签名。
            storeFile = file(signingStoreFile.get())
            storePassword = signingStorePassword.get()
            keyAlias = signingKeyAlias.get()
            keyPassword = signingKeyPassword.get()
        }
    } else {
        null
    }

    buildTypes {
        release {
            signingConfig = packagedReleaseSigning
            isMinifyEnabled = true
            // 打包器依赖唯一的 drawable-nodpi-v4/app_icon.png 槽位替换自定义图标；禁用资源名混淆避免编译器将它改名成不可预测的短名。
            isShrinkResources = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    buildFeatures {
        compose = true
    }

    externalNativeBuild {
        ndkBuild {
            path = file("src/main/cpp/Android.mk")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    androidResources {
        // 密文资料保持独立 ZIP 条目，打包器可在不触碰资源表的情况下覆盖可变长度 AEAD 容器。
        noCompress += "bin"
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
        jniLibs {
            // 客户端没有调用 AndroidX PathIterator；排除其可选回溯库，避免发布 APK 出现第二个 SO。
            // 固定 HEV 源码已静态链接进 libroutesocks，每个 ABI 的唯一 Native 产物就是该业务库；
            // release 必须由 AGP 剥离调试符号，否则每次在线生成和手机下载都会额外传输十余 MiB。
            excludes += "**/libandroidx.graphics.path.so"
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}

dependencies {
    implementation(libs.androidxCoreKtx)
    implementation(libs.androidxLifecycleRuntime)
    implementation(libs.androidxLifecycleViewModelCompose)
    implementation(libs.androidxActivityCompose)

    implementation(platform(libs.androidxComposeBom))
    implementation(libs.androidxComposeUi)
    implementation(libs.androidxComposeUiGraphics)
    implementation(libs.androidxComposeUiToolingPreview)
    implementation(libs.androidxComposeMaterial3)
    implementation(libs.androidxComposeMaterialIconsExtended)

    testImplementation(libs.junit)
    // JVM 单元测试使用与 Android 同契约的 JSONObject 实现，避免平台桩方法掩盖节点解析错误。
    testImplementation(libs.json)
    debugImplementation(libs.androidxComposeUiTooling)
}
