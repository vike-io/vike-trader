#Requires -Version 5.1
<#
.SYNOPSIS
  One-command provisioning of the Dukascopy JForex sidecar (spec slice 2b).

.DESCRIPTION
  Fresh clone -> working sidecar with zero installs:
    1. Portable Temurin 17 JRE -> bin/jre/     (download+unzip only; no MSI, no
       admin, no PATH/JAVA_HOME edits — the exec client resolves
       <project>/bin/jre/*/bin/java at RUNTIME)
    2. bin/jforex/jforex-bridge.jar is FETCHED from the GitHub Release, verified
       against that release's SHA256SUMS. It is no longer committed: it used to be
       force-added past .gitignore, which put a 43 MB binary in every clone's
       history to serve the few checkouts that trade Dukascopy.
    3. Protocol smoke: the jar must answer with the expected fatal missing-env
       envelope, proving java + jar + Main-Class all work.

  -Force instead REBUILDS the jar from the committed Java sources, which needs
  javac: it downloads a full Temurin JDK into vendor/tools/.

  ⚠ SO THIS SCRIPT NOW NEEDS NETWORK TO GITHUB (and a token — the repository is
  private), where before it needed only Adoptium. An offline clone gets no jar.
  Two escapes: JFOREX_BRIDGE_JAR names a jar OUTRIGHT and beats the project rung
  entirely (see the exec client's resolve_dukascopy_tools), or -Force builds one
  from source — strictly more network, but no release.

  ⚠ THE SPLIT — each image lands where it is needed, and the two roots are DISJOINT:

      JRE — merely RUNS the committed jar   -> RUNTIME    -> <project>/bin/jre/
      JDK — javac, and only under -Force    -> BUILD-TIME -> <workspace>/vendor/tools/

  `vendor/` exists only in a checkout, so nothing a DEPLOYED binary needs may live
  there; `bin/` is the project's runtime-tool directory
  (`vike_model::state_path::PROJECT_BIN_DIR`), resolved at runtime by the same walk
  that finds `settings/`. The jar moved for the same reason.

  The disjoint roots are also what makes the image-mixing incident recorded in
  provision-jforex.sh's find_java STRUCTURALLY impossible here: a JRE left by an
  earlier plain run is not on -Force's search path at all.

  Idempotent: an existing image/jar is reused.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File crates\bridges\dukascopy\scripts\provision-jforex.ps1
#>
param([switch]$Force)

$ErrorActionPreference = 'Stop'

# PINNED Temurin release + per-image SHA256 (2026-07 hardening: no more floating /latest/ download).
# Must match CI's setup-java pin (.github/workflows/jforex-bridge.yml java-version) and the dev
# box's vendor/tools JDK — javac output feeds the drift-gated jar.
# Checksum source: https://api.adoptium.net/v3/assets/version/17.0.19%2B10 (windows/x64, `package`).
$JdkVersion = '17.0.19+10'
$JdkSha256  = 'b5b235c48adf6a081874b812c630b9f4b5f637b7a5ed18b9174d08a41ec4c235'
$JreSha256  = '79a598e1fbb4e16582d92c4ee22280a3c4d72fd52606e1e46b1223c0fe53b0da'
# Script lives at <workspace>/crates/bridges/dukascopy/scripts/ — 4 up to the root.
$Root   = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
$Tools  = Join-Path $Root 'vendor\tools'          # build-time: the JDK whose javac compiles the sidecar
$JreDir = Join-Path $Root 'bin\jre'               # runtime: the JVM that runs the committed jar
$Jar    = Join-Path $Root 'bin\jforex\jforex-bridge.jar'
$Bridge = (Resolve-Path (Join-Path $PSScriptRoot '..\jforex-bridge')).Path # sibling — rename-proof

# -Force needs javac (full JDK); the normal path only needs a JRE to run the jar. Each image
# resolves from — and extracts into — its OWN root, per THE SPLIT above.
if ($Force) {
    $Image = 'jdk'; $ImageRoot = $Tools;  $ImageSha = $JdkSha256; $ImageSize = '~190 MB'
} else {
    $Image = 'jre'; $ImageRoot = $JreDir; $ImageSha = $JreSha256; $ImageSize = '~44 MB'
}

# Searches ONE root — the one belonging to the image being asked for. The javac check is a SECOND
# line of defence (the roots are a few lines apart and a future edit could point them at one
# directory again); `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` holds both.
function Find-ImageJava {
    Get-ChildItem -Path $ImageRoot -Directory -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName 'bin\java.exe' } |
        Where-Object { Test-Path $_ } |
        Where-Object { $Image -ne 'jdk' -or (Test-Path (Join-Path (Split-Path $_) 'javac.exe')) } |
        Sort-Object | Select-Object -Last 1
}

# --- 1. Java image (portable) --------------------------------------------------
$java = Find-ImageJava
if (-not $java) {
    New-Item -ItemType Directory -Force $ImageRoot | Out-Null
    $zip = Join-Path $ImageRoot 'temurin17.zip'
    Write-Host ">> downloading portable Temurin $JdkVersion $Image ($ImageSize, unzip-only, no install) -> $ImageRoot..."
    $uri = "https://api.adoptium.net/v3/binary/version/jdk-$([uri]::EscapeDataString($JdkVersion))/windows/x64/$Image/hotspot/normal/eclipse?project=jdk"
    Invoke-WebRequest -Uri $uri -OutFile $zip
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $ImageSha) {
        Remove-Item $zip -Confirm:$false
        throw "Temurin $Image download failed SHA256 verification (expected $ImageSha, got $actual) — refusing to extract"
    }
    Write-Host '>> SHA256 verified; extracting...'
    Expand-Archive -Path $zip -DestinationPath $ImageRoot -Force
    Remove-Item $zip -Confirm:$false
    $java = Find-ImageJava
    if (-not $java) { throw "extraction produced no usable $Image under $ImageRoot (a jdk must have bin\javac.exe beside bin\java.exe)" }
}
Write-Host ">> java: $java"
& $java -version 2>&1 | Select-Object -First 1 | Write-Host

# --- 2. Sidecar jar --------------------------------------------------------------
# The PowerShell twin of scripts/fetch_release_tools.sh, and it is a twin rather than a call
# because a Windows box is not guaranteed a bash. It keeps that script's ONE rule: nothing is
# installed before its checksum has been verified against the release's own SHA256SUMS.
function Get-JarFromRelease {
    $repo = $env:GITHUB_REPOSITORY
    if (-not $repo) {
        $url  = (& git -C $Root remote get-url origin 2>$null)
        $repo = ($url -replace '^git@github\.com:', '' -replace '^https://github\.com/', '' -replace '\.git$', '')
    }
    if ($repo -notmatch '/') { throw "could not work out the GitHub repository from origin ('$url'); set GITHUB_REPOSITORY=owner/name" }

    $tok = $env:GH_TOKEN; if (-not $tok) { $tok = $env:GITHUB_TOKEN }
    if (-not $tok -and (Get-Command gh -ErrorAction SilentlyContinue)) { $tok = (& gh auth token 2>$null) }
    if (-not $tok) {
        throw "no GitHub token. This repository is private, so a release read without one returns 404 and is indistinguishable from a missing tag. Set `$env:GH_TOKEN, or run 'gh auth login'."
    }
    $hdr = @{ Authorization = "Bearer $tok"; Accept = 'application/vnd.github+json'; 'X-GitHub-Api-Version' = '2022-11-28' }

    Write-Host ">> sidecar jar absent — fetching it from the latest release of $repo..."
    $rel = Invoke-RestMethod -Headers $hdr -Uri "https://api.github.com/repos/$repo/releases/latest"
    Write-Host ">> release $($rel.tag_name)"

    New-Item -ItemType Directory -Force (Split-Path $Jar) | Out-Null
    $stage = Join-Path (Split-Path $Jar) ".fetch-$PID"
    New-Item -ItemType Directory -Force $stage | Out-Null
    try {
        # Both files. The sums manifest is what makes the jar trustworthy; downloading the jar
        # alone would be a download, not an install.
        foreach ($name in @('SHA256SUMS', 'jforex-bridge.jar')) {
            $asset = $rel.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
            if (-not $asset) {
                throw "release $($rel.tag_name) carries no asset '$name'. A release cut BEFORE the runtime tools were attached will not have one — deploy a newer tag, or build the jar locally with -Force."
            }
            # A NEW hashtable, not `$hdr + @{...}`: `$hdr` already carries an `Accept` key and
            # hashtable addition throws on a duplicate. The asset endpoint needs octet-stream to
            # return bytes instead of JSON metadata (the private-repo redirect dance deploy.yml
            # documents).
            $dl = @{ Authorization = $hdr.Authorization; Accept = 'application/octet-stream';
                     'X-GitHub-Api-Version' = $hdr.'X-GitHub-Api-Version' }
            Invoke-WebRequest -Headers $dl `
                -Uri "https://api.github.com/repos/$repo/releases/assets/$($asset.id)" `
                -OutFile (Join-Path $stage $name)
        }
        $want = (Select-String -Path (Join-Path $stage 'SHA256SUMS') -Pattern '\s+jforex-bridge\.jar$' |
                 Select-Object -First 1).Line
        if (-not $want) { throw "the release's SHA256SUMS does not list jforex-bridge.jar — refusing to install an unverifiable file" }
        $expected = ($want -split '\s+')[0].ToLower()
        $actual   = (Get-FileHash (Join-Path $stage 'jforex-bridge.jar') -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            throw "jforex-bridge.jar failed SHA256 verification (expected $expected, got $actual) — refusing to install"
        }
        Write-Host '>> SHA256 verified'
        # rename(2) replaces the destination on POSIX but FAILS on Windows when it exists.
        if (Test-Path $Jar) { Remove-Item $Jar -Force -Confirm:$false }
        Move-Item (Join-Path $stage 'jforex-bridge.jar') $Jar
        Write-Host ">> installed $Jar"
    } finally { Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue }
}

if (-not $Force) {
    if (Test-Path $Jar) {
        Write-Host ">> jar already present: $Jar   (use -Force to build one from source)"
    } else {
        Get-JarFromRelease
    }
} else {
    $env:JAVA_HOME = Split-Path (Split-Path $java)
    Write-Host '>> building sidecar (gradlew shadowJar; first run bootstraps Gradle once)...'
    Push-Location $Bridge
    try {
        & .\gradlew.bat --no-daemon -q shadowJar
        if ($LASTEXITCODE -ne 0) { throw "gradle build failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
    New-Item -ItemType Directory -Force (Split-Path $Jar) | Out-Null
    Copy-Item (Join-Path $Bridge 'build\libs\jforex-bridge-all.jar') $Jar -Force
    # ⚠ NOT "remember to commit it" any more — the jar is gitignored, and what a release ships is
    # built by .github/workflows/release.yml from these same sources. A local build is for TESTING
    # an edit; the way to publish one is to merge the Java change and cut a release.
    Write-Host ">> built: $Jar   (local only — the shipped jar is built by release.yml from these sources)"
}

# --- 3. Protocol smoke -----------------------------------------------------------
# With no DUKASCOPY_* env the sidecar must emit exactly one fatal envelope and exit.
$probe = & $java -jar $Jar 2>$null | Select-Object -First 1
if ($probe -match '"kind":"fatal"') {
    Write-Host '>> smoke OK — sidecar launches and speaks the protocol'
} else {
    Write-Warning "unexpected sidecar probe output: $probe"
}

Write-Host ''
Write-Host 'DONE. Nothing was installed; the exec client resolves <project>\bin\jre at runtime.'
Write-Host 'Remove the runtime with: Remove-Item -Recurse -Force bin\jre   (the build-time JDK lives in vendor\tools)'
