import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// versionCode is the build number and versionName the build's version, both from the build
// hook (.agents/hooks/build), so the APK matches its build/<n> release.
val buildNumber = (System.getenv("BUILD_NUMBER") ?: "1").toInt()
val appVersion = providers.gradleProperty("appVersion").getOrElse("dev-$buildNumber")

android {
    namespace = "gitstat.phone"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.oleroma.gitstat"
        minSdk = 26
        targetSdk = 35
        versionCode = buildNumber
        versionName = appVersion
    }

    // One key for every build, PC or CI, so Obtainium can update the app from any of them.
    // It only signs this sideloaded app; it guards nothing else.
    signingConfigs {
        create("gitstat") {
            storeFile = file("gitstat-phone.keystore")
            storePassword = "gitstat-phone"
            keyAlias = "gitstat"
            keyPassword = "gitstat-phone"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("gitstat")
        }
        debug {
            signingConfig = signingConfigs.getByName("gitstat")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { compose = true }

    lint {
        abortOnError = true
        checkReleaseBuilds = false
    }
}

kotlin { compilerOptions { jvmTarget.set(JvmTarget.JVM_17) } }

dependencies {
    implementation(project(":core"))
    implementation("androidx.core:core-ktx:1.16.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation(platform("androidx.compose:compose-bom:2025.05.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.glance:glance-appwidget:1.1.1")
    implementation("androidx.work:work-runtime-ktx:2.10.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
}
