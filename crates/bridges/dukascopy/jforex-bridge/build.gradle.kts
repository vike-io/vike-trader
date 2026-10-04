// JForex sidecar for vike-trader-rust (spec: docs/superpowers/specs/2026-07-04-dukascopy-jforex-bridge-design.md).
// Build (slice 2b, needs a JDK 17+): gradle wrapper && ./gradlew shadowJar
// then copy build/libs/jforex-bridge-all.jar -> ../../../../vendor/jforex-bridge.jar (workspace vendor/)
plugins {
    java
    id("com.gradleup.shadow") version "8.3.6"
}

java {
    toolchain { languageVersion = JavaLanguageVersion.of(17) }
}

repositories {
    mavenCentral()
    maven { url = uri("https://www.dukascopy.com/client/jforexlib/publicrepo/") }
}

dependencies {
    // Verified live 2026-07-04; the client artifact rotates — bump the pin when 2b builds.
    implementation("com.dukascopy.api:JForex-API:2.13.99")
    implementation("com.dukascopy.dds2:DDS2-jClient-JForex:3.6.51")
    implementation("com.google.code.gson:gson:2.11.0")
    testImplementation("org.junit.jupiter:junit-jupiter:5.10.2")
}

// JDK 17 javac defaults to the PLATFORM charset (Cp1252 on Windows, UTF-8 on Linux):
// unpinned, the same source produced different constant pools per OS — Windows builds
// mojibake'd the em-dashes inside error-string literals (caught by the CI drift gate).
tasks.withType<JavaCompile>().configureEach { options.encoding = "UTF-8" }

tasks.test { useJUnitPlatform() }

tasks.jar { manifest { attributes["Main-Class"] = "vike.jforex.Bridge" } }

// Byte-reproducible archives: same sources + same JDK => bit-identical jar. This is
// what lets CI (jforex-bridge.yml, the CI box runner) rebuild and `cmp` against the
// committed vendor/jforex-bridge.jar — the drift gate for "jar matches source".
tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
