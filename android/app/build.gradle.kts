plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// The version is the product version of Cargo.toml (tools/version.sh), passed by tools/android/build-apk.sh.
val inboxVersion: String = (findProperty("inboxVersion") as String?)
    ?: error("-PinboxVersion=<x.y.z> is required (use tools/android/build-apk.sh)")
val versionParts = inboxVersion.split(".").map { it.takeWhile(Char::isDigit).toInt() }
require(versionParts.size == 3) { "inboxVersion must be x.y.z" }

fun env(name: String): String =
    System.getenv(name)?.takeIf { it.isNotBlank() } ?: error("$name is required to sign the release APK")

android {
    namespace = "app.portalinbox"
    compileSdk = 35
    buildToolsVersion = "35.0.0"

    defaultConfig {
        applicationId = "app.portalinbox"
        minSdk = 26
        targetSdk = 35
        versionCode = versionParts[0] * 10000 + versionParts[1] * 100 + versionParts[2]
        versionName = inboxVersion
        ndk { abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64") }
        // The app is English only. AndroidX brings strings in about 80 languages that nothing would show: leave them out.
        resourceConfigurations += listOf("en")
    }

    signingConfigs {
        create("release") {
            if (gradle.startParameter.taskNames.any { it.contains("Release", ignoreCase = true) }) {
                storeFile = file(env("INBOX_KEYSTORE"))
                storePassword = env("INBOX_KEYSTORE_PASSWORD")
                keyAlias = env("INBOX_KEY_ALIAS")
                keyPassword = env("INBOX_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    buildFeatures { buildConfig = true }
    packaging { jniLibs { useLegacyPackaging = false } }
    lint {
        abortOnError = true
        checkReleaseBuilds = true
        disable += listOf("OldTargetApi", "GradleDependency", "NewerVersionAvailable")
    }
}

dependencies {
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    // BiometricPrompt: the local gate before an Approve (fingerprint, face or device PIN). The server cannot check it.
    implementation("androidx.biometric:biometric:1.1.0")
    // The unit tests run on the JVM. The Android `org.json` is a stub there: use the real library.
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20260814")
}
