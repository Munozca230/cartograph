<#
.SYNOPSIS
  Build the Windows release binary with the embedded web UI and package it locally.

.DESCRIPTION
  1. web: npm ci + npm run build (web/dist), unless -SkipWeb and web/dist exists.
  2. cargo build --release for x86_64-pc-windows-msvc (static CRT, see .cargo/config.toml),
     with CARTOGRAPH_WEB_DIST pointing at web/dist so build.rs embeds it.
  3. `cartograph doctor` must report the web UI as embedded.
  4. dist/cartograph-<version>-x86_64-pc-windows-msvc.zip (the binary, README.md, the
     license files and THIRD_PARTY_NOTICES.md) and its .sha256.

  Nothing is published: no tag, no upload, no release.

.EXAMPLE
  pwsh scripts/package.ps1
  powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -Test
#>
param(
  # Reuse an existing web/dist instead of rebuilding it.
  [switch]$SkipWeb,
  # Run `cargo test --workspace --release` before packaging.
  [switch]$Test
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

function Invoke-Checked([string]$what, [scriptblock]$block) {
  & $block
  if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" }
}

Push-Location $root
try {
  # The workspace version ([workspace.package] in the root Cargo.toml).
  $versionLine = Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1
  if (-not $versionLine) { throw 'no version in Cargo.toml' }
  $version = $versionLine.Matches[0].Groups[1].Value
  $name = "cartograph-$version-$target"
  Write-Host "packaging $name"

  $dist = Join-Path $root 'web/dist'
  if (-not ($SkipWeb -and (Test-Path (Join-Path $dist 'index.html')))) {
    Push-Location web
    try {
      Invoke-Checked 'npm ci' { npm ci }
      Invoke-Checked 'npm run build' { npm run build }
    } finally { Pop-Location }
  }
  if (-not (Test-Path (Join-Path $dist 'index.html'))) { throw "web/dist/index.html is missing" }

  # build.rs re-scans when this variable changes, so a first web build is always picked up.
  $env:CARTOGRAPH_WEB_DIST = (Resolve-Path $dist).Path
  $env:CARGO_INCREMENTAL = '0'
  if ($Test) {
    Invoke-Checked 'cargo test' { cargo test --workspace --locked --release --target $target }
  }
  Invoke-Checked 'cargo build' { cargo build --release --locked -p cartograph-cli --target $target }
  $exe = Join-Path $root "target/$target/release/cartograph.exe"

  $probe = Join-Path ([System.IO.Path]::GetTempPath()) ("cartograph-doctor-" + [guid]::NewGuid())
  New-Item -ItemType Directory -Path $probe | Out-Null
  try {
    $doctor = & $exe doctor $probe | Out-String
    Write-Host $doctor
    if ($doctor -notmatch 'web ui\s+embedded in this binary') { throw 'the web UI is not embedded' }
  } finally { Remove-Item -Recurse -Force $probe }

  $out = Join-Path $root 'dist'
  $stage = Join-Path $out $name
  if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
  New-Item -ItemType Directory -Path $stage | Out-Null
  Copy-Item $exe $stage
  foreach ($doc in 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md') {
    if (-not (Test-Path (Join-Path $root $doc))) { throw "$doc is missing" }
    Copy-Item (Join-Path $root $doc) $stage
  }
  $zip = Join-Path $out "$name.zip"
  if (Test-Path $zip) { Remove-Item -Force $zip }
  Compress-Archive -Path $stage -DestinationPath $zip
  Remove-Item -Recurse -Force $stage

  $hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
  # Same format as sha256sum: "<hash>  <file>".
  [System.IO.File]::WriteAllText("$zip.sha256", "$hash  $name.zip`n")
  $size = (Get-Item $zip).Length
  $exeSize = (Get-Item $exe).Length
  Write-Host ("{0}  {1:N1} MB (binary {2:N1} MB)" -f $zip, ($size / 1MB), ($exeSize / 1MB))
  Write-Host "sha256 $hash"
} finally {
  Pop-Location
}
