# Zip the plugin tools\build_plugin.ps1 built (target\plugin) for a release:
# target\plugin\release\fm1-plugin-<version>-windows-x86_64.zip with the VST3 (a folder), the CLAP
# and an install guide. The build is not code-signed; Windows hosts load it as it is.
#   tools\package_plugin.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$out = 'target\plugin'
$version = (Select-String -Path 'crates\fm1-plugin\Cargo.toml' -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$name = "fm1-plugin-$version-windows-x86_64"
$rel = "$out\release"
Remove-Item $rel -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$rel\$name" | Out-Null
foreach ($b in 'FM-1.vst3', 'FM-1.clap') {
    if (-not (Test-Path "$out\$b")) { throw "no $out\${b}: run tools\build_plugin.ps1 first" }
    Copy-Item "$out\$b" "$rel\$name\$b" -Recurse
}

$install = @"
FM-1 plugin ${version}: the M-VAVE FM-1 emulator as an instrument (VST3, CLAP)
https://github.com/joakimeriksson/fm1-emu

For Windows 10 or 11, 64-bit (x86-64).

1. Copy both plug-ins to your plug-in folders (create the folders that are missing):
     FM-1.vst3  (the whole folder)  ->  C:\Program Files\Common Files\VST3\
     FM-1.clap                      ->  C:\Program Files\Common Files\CLAP\
   Without administrator rights, the same folders under your own profile work too:
     C:\Users\<you>\AppData\Local\Programs\Common\VST3\  and  ...\Common\CLAP\
   (REAPER and Waveform scan those; some DAWs only scan Program Files.)
   Keep both: the VST3 loads FM-1.clap from the CLAP folder when a DAW opens it.
2. This build is not code-signed. Windows may warn when you open the zip; the plug-ins
   themselves load without a prompt.
3. Restart your DAW, or rescan its plug-ins. Add "FM-1" (by fm1-emu) as an instrument on a track.
4. In the plug-in's window, click "Load firmware / save..." and pick a .fwsc package: a community
   firmware (the list, with each author's link: https://joakimeriksson.github.io/fm1-emu/)
   or M-VAVE's own. Play it from MIDI or with the mouse on its panel.

The firmware and the patches it saves are kept in your project.
"@
Set-Content -Path "$rel\$name\INSTALL.txt" -Value $install

Compress-Archive -Path "$rel\$name" -DestinationPath "$rel\$name.zip"
Remove-Item "$rel\$name" -Recurse -Force
Write-Host "packaged $rel\$name.zip (unsigned)"
