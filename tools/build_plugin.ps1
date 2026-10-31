# Build the FM-1 plugin (Windows, x86-64): the CLAP from Rust (crates/fm1-plugin), then the VST3
# around it (clap-wrapper, crates/fm1-plugin/wrap). Unlike the macOS bundles, the Windows VST3
# doesn't carry the CLAP: it loads FM-1.clap at run time from a CLAP folder (Common Files\CLAP,
# %LOCALAPPDATA%\Programs\Common\CLAP, or a folder in CLAP_PATH), so both files are installed.
#   tools\build_plugin.ps1             # build, install in %LOCALAPPDATA%\Programs\Common\{VST3,CLAP}
#   $env:NO_INSTALL = 1; tools\build_plugin.ps1   # only build, into target\plugin\
# Needs Rust (MSVC), Visual Studio Build Tools (C++ workload), CMake and git:
#   winget install Rustlang.Rustup Kitware.CMake Git.Git
#   winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$root = (Get-Location).Path
$out = Join-Path $root 'target\plugin'
New-Item -ItemType Directory -Force $out | Out-Null

# the CLAP: the cdylib, renamed
Push-Location crates\fm1-plugin
cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
Pop-Location
$clap = Join-Path $out 'FM-1.clap'
Copy-Item crates\fm1-plugin\target\release\fm1_plugin.dll $clap -Force

# the VST3 around it (the Visual Studio generator finds the Build Tools by itself)
cmake -S crates\fm1-plugin\wrap -B "$out\build" -G "Visual Studio 17 2022" -A x64 -DFM1_CLAP="$clap"
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cmake --build "$out\build" --config Release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$vst3 = Get-ChildItem "$out\build\Release" -Directory -Filter 'FM-1.vst3' | Select-Object -First 1
if (-not $vst3) { throw "no FM-1.vst3 under $out\build\Release" }
Remove-Item "$out\FM-1.vst3" -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $vst3.FullName "$out\FM-1.vst3" -Recurse
Write-Host "built $clap and $out\FM-1.vst3"

if ($env:NO_INSTALL) { exit 0 }
# the per-user plugin folders (no administrator needed): hosts scan Programs\Common\VST3, and
# clap-wrapper and CLAP hosts scan Programs\Common\CLAP
$dst = Join-Path $env:LOCALAPPDATA 'Programs\Common'
New-Item -ItemType Directory -Force "$dst\VST3", "$dst\CLAP" | Out-Null
Remove-Item "$dst\VST3\FM-1.vst3", "$dst\CLAP\FM-1.clap" -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item "$out\FM-1.vst3" "$dst\VST3\FM-1.vst3" -Recurse
Copy-Item $clap "$dst\CLAP\FM-1.clap"
Write-Host "installed in $dst\VST3 and $dst\CLAP"
