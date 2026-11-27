// Everything Gradle makes goes in the repo's build/ folder (AGENTS.md "Builds").
subprojects {
    layout.buildDirectory.set(rootDir.parentFile.resolve("build/android/$name"))
}
