# Builds Factory Builder: the egui UI to wasm (into dist/), the icon helper, then the app.
#   .\build.ps1            development build -> target\release\factory-builder.exe and
#                          target\release\tools\ (has Developer settings: test first launch etc.)
#   .\build.ps1 -UiOnly    rebuild only the wasm UI (the app embeds dist\, so run a full
#                          build before trying UI changes in the app)
#   .\build.ps1 -Package   shipping build without Developer settings, in
#                          target\package\Factory Builder\ and target\package\FactoryBuilder-<version>.zip
#                          (first launch installs it; see src-tauri\src\install.rs)
param([switch]$UiOnly, [switch]$Package)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# Rust embeds source paths (for panic messages). Map this PC's user folder to
# "~" so nothing built here carries the Windows account name. Restored at the
# end so it doesn't leak into the calling shell.
$oldFlags = $env:CARGO_ENCODED_RUSTFLAGS
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$($env:USERPROFILE)=~"
try {
    $bindgenVersion = (cargo tree -p fb-ui --target wasm32-unknown-unknown -i wasm-bindgen --depth 0 | Select-Object -First 1) -replace '^wasm-bindgen v', ''
    $have = (& wasm-bindgen --version 2>$null) -replace '^wasm-bindgen ', ''
    if ($have -ne $bindgenVersion) {
        throw "wasm-bindgen CLI $have doesn't match the wasm-bindgen crate $bindgenVersion. Run: cargo install wasm-bindgen-cli --version $bindgenVersion --locked"
    }

    cargo build -p fb-ui --target wasm32-unknown-unknown --release
    if ($LASTEXITCODE) { throw 'wasm build failed' }
    New-Item -ItemType Directory -Force dist | Out-Null
    wasm-bindgen --target web --no-typescript --out-dir dist target\wasm32-unknown-unknown\release\fb_ui.wasm
    if ($LASTEXITCODE) { throw 'wasm-bindgen failed' }
    Copy-Item web\index.html dist\index.html -Force
    if ($UiOnly) { return }

    # The icon helper: one self-contained exe, so players don't need .NET installed.
    dotnet publish tools\IconExtractor -c Release -r win-x64 --self-contained --nologo -v q `
        -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true `
        -p:EnableCompressionInSingleFile=true -p:DebugType=none -o target\helper
    if ($LASTEXITCODE) { throw 'icon helper build failed' }
    # Its open-source Oodle decoder.
    cargo build -p oodle-shim --release
    if ($LASTEXITCODE) { throw 'oodle-shim build failed' }

    $appArgs = @('build', '-p', 'factory-builder', '--release')
    if (-not $Package) { $appArgs += @('--features', 'dev') }
    cargo @appArgs
    if ($LASTEXITCODE) {
        # Antivirus sometimes holds the freshly linked exe for a moment; a
        # second try then succeeds. Real compile errors fail again.
        Write-Host 'App build failed; trying once more.'
        cargo @appArgs
    }
    if ($LASTEXITCODE) { throw 'app build failed' }
    New-Item -ItemType Directory -Force target\release\tools | Out-Null
    Copy-Item target\helper\IconExtractor.exe, target\release\oodle_shim.dll target\release\tools -Force

    if (-not $Package) {
        Write-Host 'Built target\release\factory-builder.exe (development build, with tools\)'
        return
    }

    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
    $out = 'target\package'
    $app = Join-Path $out 'Factory Builder'
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    New-Item -ItemType Directory -Force (Join-Path $app 'tools') | Out-Null
    Copy-Item target\release\factory-builder.exe (Join-Path $app 'Factory Builder.exe')
    Copy-Item target\release\tools\* (Join-Path $app 'tools')

    # Licenses of what the icon helper bundles, read from each package's own metadata.
    $lines = @('Factory Builder bundles the following third-party software in tools\.', '',
        'oodle_shim.dll: built on oozextract 0.5.7 (MIT), https://crates.io/crates/oozextract', '',
        'IconExtractor.exe: the .NET runtime (MIT) and these NuGet packages:')
    $nuget = Join-Path $env:USERPROFILE '.nuget\packages'
    $listed = dotnet list tools\IconExtractor package --include-transitive | Select-String '^\s+>\s+(\S+)\s+(?:\S+\s+)?(\S+)\s*$'
    foreach ($m in $listed) {
        $name = $m.Matches[0].Groups[1].Value
        $ver = $m.Matches[0].Groups[2].Value
        # In the dependency graph but not shipped (see IconExtractor.csproj).
        if ($name -eq 'SixLabors.ImageSharp') { continue }
        $spec = Get-ChildItem (Join-Path $nuget "$($name.ToLower())\$ver") -Filter *.nuspec -ErrorAction SilentlyContinue | Select-Object -First 1
        $license = 'see package'
        if ($spec) {
            [xml]$x = Get-Content $spec.FullName
            $md = $x.package.metadata
            if ($md.license -and $md.license.type -eq 'expression') { $license = $md.license.'#text' }
            elseif ($md.license) { $license = "license file in package ($($md.projectUrl))" }
            elseif ($md.licenseUrl) { $license = $md.licenseUrl }
        }
        $lines += "  $name $ver - $license"
    }
    $lines | Set-Content -Encoding utf8 (Join-Path $app 'THIRD-PARTY-NOTICES.txt')

    $zip = Join-Path $out "FactoryBuilder-$version.zip"
    Compress-Archive -Path $app -DestinationPath $zip
    Write-Host "Packaged $app and $zip"
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $oldFlags
}
