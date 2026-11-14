// JForex sidecar for vike-trader-rust (spec: docs/superpowers/specs/2026-07-04-dukascopy-jforex-bridge-design.md).
// Build (slice 2b, needs a JDK 17+): gradle wrapper && ./gradlew shadowJar
// then copy build/libs/jforex-bridge-all.jar -> ../../../../vendor/jforex-bridge.jar (workspace vendor/)
import java.security.MessageDigest

plugins {
    java
    id("com.gradleup.shadow") version "8.3.6"
}

java {
    toolchain { languageVersion = JavaLanguageVersion.of(17) }
}

// The JForex SDK, and every other jar the shipped archive packs, come from a vendor FOLDER of exactly the jars
// runtime-classpath.sha256 names: the vendor's Maven repository answered 404 on 2026-11-08 (it answers again since
// 2026-11-09, with the same files; the build does not use it) and the SDK is proprietary, so it is neither a repository
// nor in git. JFOREX_SDK_DIR names the folder, as FCSDK_DIR names the FXCM SDK; unset, it is
// the workspace's gitignored vendor/jforex.
val jforexSdk = file(System.getenv("JFOREX_SDK_DIR") ?: "../../../../vendor/jforex")
if (!jforexSdk.isDirectory) {
    throw GradleException("no JForex SDK folder at $jforexSdk (JFOREX_SDK_DIR): it holds the jars runtime-classpath.sha256 names")
}
// `<sha256>  <group>:<name>:<version>/<file>` rows. No POM survives, so nothing is transitive: these rows ARE the list.
val runtimePins = file("runtime-classpath.sha256")
val pinRows = runtimePins.readLines().filter { it.isNotBlank() && !it.startsWith("#") }
    .map { it.trim().split(Regex("\\s+"), limit = 2) }

repositories {
    // A pinned module is looked for in the folder only, so a missing jar fails rather than coming from somewhere else.
    exclusiveContent {
        forRepository { flatDir { dirs(jforexSdk) } }
        filter { pinRows.forEach { (_, key) -> key.split(':').let { (group, name) -> includeModule(group, name) } } }
    }
    mavenCentral()
}

// One spelling of the API pin, the dependency below.
val jforexApiVersion = "2.13.99"

dependencies {
    // One per row, in the rows' order (the classpath's, which is the jar's bytes); `<name>-<version>-<classifier>.jar` is
    // that classifier's file.
    pinRows.forEach { (_, key) ->
        val (coord, jar) = key.split('/', limit = 2)
        val (_, name, version) = coord.split(':')
        val classifier = jar.removeSuffix(".jar").removePrefix("$name-$version").removePrefix("-")
        implementation(if (classifier.isEmpty()) coord else "$coord:$classifier")
    }
    // What the sources use, among the rows above; a version here that is not a row's fails the classpath check.
    // Verified live 2026-07-04; the client artifact rotates — bump the pin when 2b builds.
    implementation("com.dukascopy.api:JForex-API:$jforexApiVersion")
    implementation("com.dukascopy.dds2:DDS2-jClient-JForex:3.6.51")
    implementation("com.google.code.gson:gson:2.11.0")
    // SdkLogging caps the SDK's log4j (the slf4j binding the SDK logs through) at WARN.
    implementation("log4j:log4j:1.2.17")
    testImplementation("org.junit.jupiter:junit-jupiter:5.10.2")
}

// JDK 17 javac defaults to the PLATFORM charset (Cp1252 on Windows, UTF-8 on Linux):
// unpinned, the same source produced different constant pools per OS — Windows builds
// mojibake'd the em-dashes inside error-string literals (caught by the CI drift gate).
tasks.withType<JavaCompile>().configureEach { options.encoding = "UTF-8" }

tasks.test {
    useJUnitPlatform()
    // NetPositionTest: the golden legs the Rust from_legs test reads too (a test input, not in the jar).
    systemProperty("vike.positionLegs", file("../data/position_legs.json").absolutePath)
}

tasks.jar { manifest { attributes["Main-Class"] = "vike.jforex.Bridge" } }

// Every jar the shipped archive is packed from is held to runtime-classpath.sha256, both ways and in order, before
// shadowJar runs: a jar whose bytes differ, one the file does not name, one it names that was not resolved and a
// classpath in another order each fail the build, and so does a file in the SDK folder the file does not name. Gradle's
// own dependency verification cannot do this: it needs the POM files, which no repository held on 2026-11-08 (the
// vendor's serves them again since 2026-11-09; the build reads none).
val runtimeJars = configurations.runtimeClasspath
tasks.shadowJar {
    inputs.file(runtimePins)
    inputs.dir(jforexSdk) // a stray file must re-run the check below, not leave the task UP-TO-DATE
    doFirst {
        val want = pinRows.associate { (sum, jar) -> jar to sum }
        val have = runtimeJars.get().incoming.artifacts.artifacts.associate { a ->
            "${a.id.componentIdentifier.displayName}/${a.file.name}" to
                MessageDigest.getInstance("SHA-256").digest(a.file.readBytes())
                    .joinToString("") { "%02x".format(it) }
        }
        val bad = (want.keys + have.keys).sorted().filter { want[it] != have[it] }
            .map { "$it: pinned ${want[it] ?: "nothing"}, resolved ${have[it] ?: "nothing"}" } +
            (jforexSdk.list().orEmpty().toSet() - want.keys.map { it.substringAfterLast('/') }.toSet()).sorted()
                .map { "$jforexSdk/$it: in the SDK folder, pinned nothing" } +
            listOfNotNull(("resolved in this order:\n" + have.keys.joinToString("\n"))
                .takeIf { want.keys.toList() != have.keys.toList() })
        if (bad.isNotEmpty()) {
            throw GradleException("the runtime classpath is not runtime-classpath.sha256:\n" + bad.joinToString("\n"))
        }
    }
}

// Byte-reproducible archives: same sources + same JDK => bit-identical jar. This is
// what lets CI (jforex-bridge.yml, the CI box runner) rebuild and `cmp` against the
// committed vendor/jforex-bridge.jar — the drift gate for "jar matches source".
tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
    // Unpinned, an entry's mode is the build tree's, i.e. the builder's umask: 002 and 022 made two jars (2026-11-08).
    filePermissions { unix("rw-r--r--") }
    dirPermissions { unix("rwxr-xr-x") }
}
