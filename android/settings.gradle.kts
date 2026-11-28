// gitstat for phone: the GitHub-only view of Olex's repos, as an Android app and widget.
// :core is plain Kotlin (the GitHub reader and the colours) so it builds and tests anywhere;
// :app is the Android app. GITSTAT_CORE_ONLY=1 leaves :app out, for machines without the
// Android SDK or Google's Maven.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        id("com.android.application") version "8.10.1"
        id("org.jetbrains.kotlin.android") version "2.1.21"
        id("org.jetbrains.kotlin.jvm") version "2.1.21"
        id("org.jetbrains.kotlin.plugin.compose") version "2.1.21"
        id("org.jetbrains.kotlin.plugin.serialization") version "2.1.21"
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "gitstat-phone"
include(":core")
if (System.getenv("GITSTAT_CORE_ONLY") == null) include(":app")
