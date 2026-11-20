param(
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin')]
    [string] $Target,

    [string] $Binary,

    [string] $PdfiumArchive,

    [string] $MakeNsi,

    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Target) {
    $hostLine = rustc -vV | Where-Object { $_ -like 'host: *' }
    if ($LASTEXITCODE -ne 0 -or -not $hostLine) {
        throw 'Could not determine the Rust host target.'
    }
    $Target = $hostLine.Substring(6)
}

$platforms = @{
    'x86_64-pc-windows-msvc' = 'win-x64'
    'aarch64-pc-windows-msvc' = 'win-arm64'
    'x86_64-unknown-linux-gnu' = 'linux-x64'
    'aarch64-unknown-linux-gnu' = 'linux-arm64'
    'x86_64-apple-darwin' = 'mac-x64'
    'aarch64-apple-darwin' = 'mac-arm64'
}
$platform = $platforms[$Target]
if (-not $platform) { throw "Unsupported target $Target." }
$windowsTarget = $platform -like 'win-*'
$macTarget = $platform -like 'mac-*'
if ($MakeNsi -and -not $windowsTarget) { throw 'NSIS packages Windows targets only.' }
$binaryName = if ($windowsTarget) { 'tinypdf.exe' } else { 'tinypdf' }

if ($Binary) {
    $Binary = (Resolve-Path -LiteralPath $Binary).Path
} else {
    $metadataText = & cargo metadata --locked --no-deps --format-version 1 --manifest-path (Join-Path $root 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Could not read Cargo metadata.' }
    $cargoTarget = ($metadataText | ConvertFrom-Json).target_directory
    $previousArchive = $env:TINYPDF_PDFIUM_ARCHIVE
    try {
        if ($windowsTarget -and $PdfiumArchive) {
            $env:TINYPDF_PDFIUM_ARCHIVE = (Resolve-Path -LiteralPath $PdfiumArchive).Path
        }
        & cargo build --manifest-path (Join-Path $root 'Cargo.toml') --release --locked --target $Target
        if ($LASTEXITCODE -ne 0) { throw "Cargo build failed for $Target." }
    } finally {
        $env:TINYPDF_PDFIUM_ARCHIVE = $previousArchive
    }
    $Binary = Join-Path $cargoTarget "$Target/release/$binaryName"
}
$binaryFile = Get-Item -LiteralPath $Binary
if ($binaryFile.PSIsContainer) { throw "$Binary must be an executable file." }
$stream = [System.IO.File]::OpenRead($binaryFile.FullName)
$reader = [System.IO.BinaryReader]::new($stream)
try {
    if ($windowsTarget) {
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw "$Binary is not a Windows executable." }
        $stream.Position = 0x3C
        $stream.Position = $reader.ReadUInt32()
        if ($reader.ReadUInt32() -ne 0x4550) { throw "$Binary lacks a PE header." }
        $expected = if ($platform -eq 'win-arm64') { 0xAA64 } else { 0x8664 }
        $machine = $reader.ReadUInt16()
    } elseif ($macTarget) {
        if ($reader.ReadUInt32() -ne [uint32]0xFEEDFACFUL) { throw "$Binary lacks a 64-bit Mach-O header." }
        $expected = if ($platform -eq 'mac-arm64') { 0x0100000C } else { 0x01000007 }
        $machine = $reader.ReadUInt32()
    } else {
        if ($reader.ReadUInt32() -ne 0x464C457F -or $reader.ReadUInt16() -ne 0x0102) {
            throw "$Binary lacks a little-endian 64-bit ELF header."
        }
        $stream.Position = 18
        $expected = if ($platform -eq 'linux-arm64') { 183 } else { 62 }
        $machine = $reader.ReadUInt16()
    }
    if ($machine -ne $expected) { throw "$Binary does not match $Target." }
} finally {
    $reader.Dispose()
}

$pdfium = & (Join-Path $PSScriptRoot 'prepare-pdfium.ps1') -Platform $platform -Archive $PdfiumArchive
$pdfiumRoot = Split-Path -Parent (Split-Path -Parent $pdfium)
$name = "TinyPDF-$platform"
$extension = if ($windowsTarget) { 'zip' } else { 'tar.gz' }
$buildRoot = Join-Path $root 'target'
$dist = if ($OutputDirectory) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
} else {
    Join-Path $root 'dist'
}
$archive = Join-Path $dist "$name.$extension"
$installer = if ($MakeNsi) {
    Join-Path $dist $(if ($platform -eq 'win-x64') { 'TinyPDF-Setup.exe' } else { "$name-setup.exe" })
}
foreach ($outputFile in @($archive, $installer) | Where-Object { $_ }) {
    if (Test-Path -LiteralPath $outputFile) { throw "Output already exists: $outputFile. Choose a new output directory." }
}
$stage = Join-Path $buildRoot "package-$([guid]::NewGuid())"
$payload = Join-Path $stage $name
$stagedArchive = Join-Path $stage "$name.$extension"

New-Item -ItemType Directory -Path $dist, $payload -Force | Out-Null
try {
    $binaryDir = $payload
    $libraryDir = $payload
    $noticesDir = Join-Path $payload 'PDFium'
    if ($macTarget) {
        $bundle = Join-Path $payload 'TinyPDF.app/Contents'
        $binaryDir = Join-Path $bundle 'MacOS'
        $libraryDir = Join-Path $bundle 'Frameworks'
        $noticesDir = Join-Path $bundle 'Resources/PDFium'
        New-Item -ItemType Directory -Path $binaryDir, $libraryDir, $noticesDir -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'macos-Info.plist') -Destination (Join-Path $bundle 'Info.plist')
    } else {
        New-Item -ItemType Directory -Path $noticesDir | Out-Null
    }
    $stagedBinary = Join-Path $binaryDir $binaryName
    $stagedLibrary = Join-Path $libraryDir (Split-Path -Leaf $pdfium)
    Copy-Item -LiteralPath $binaryFile.FullName -Destination $stagedBinary
    Copy-Item -LiteralPath $pdfium -Destination $stagedLibrary
    Copy-Item -LiteralPath (Join-Path $pdfiumRoot 'LICENSE'), (Join-Path $pdfiumRoot 'VERSION') -Destination $noticesDir
    Copy-Item -LiteralPath (Join-Path $pdfiumRoot 'licenses') -Destination $noticesDir -Recurse
    if (-not $IsWindows) {
        & chmod 755 $stagedBinary
        if ($LASTEXITCODE -ne 0) { throw 'Could not make the packaged app executable.' }
    }
    if ($macTarget -and $IsMacOS) {
        & codesign --force --sign - $stagedLibrary
        if ($LASTEXITCODE -ne 0) { throw 'Could not sign the macOS PDFium library.' }
        & codesign --force --sign - (Join-Path $payload 'TinyPDF.app')
        if ($LASTEXITCODE -ne 0) { throw 'Could not sign the macOS application bundle.' }
    }
    if ($windowsTarget) {
        Compress-Archive -LiteralPath $payload -DestinationPath $stagedArchive -CompressionLevel Optimal
    } else {
        & tar -czf $stagedArchive -C $stage $name
        if ($LASTEXITCODE -ne 0) { throw "Could not create $name.$extension." }
    }
    if ($MakeNsi) {
        $info = $binaryFile.VersionInfo
        if (-not $info.ProductVersion) { throw 'The Windows executable lacks version information.' }
        $version = "$($info.ProductMajorPart).$($info.ProductMinorPart).$($info.ProductBuildPart)"
        $stagedInstaller = Join-Path $stage 'TinyPDF-Setup.exe'
        & $MakeNsi '/WX' "/DPAYLOAD=$payload" "/DOUTPUT=$stagedInstaller" "/DPRODUCT_VERSION=$version" "/DRELEASE_LABEL=$($info.ProductVersion)" (Join-Path $PSScriptRoot 'windows-install.nsi') | Out-Host
        if ($LASTEXITCODE -ne 0) { throw 'NSIS failed to build the installer.' }
        [System.IO.File]::Move($stagedInstaller, $installer)
    }
    [System.IO.File]::Move($stagedArchive, $archive)

    [PSCustomObject] @{
        Target = $Target
        BinaryBytes = (Get-Item -LiteralPath $stagedBinary).Length
        PdfiumBytes = (Get-Item -LiteralPath $stagedLibrary).Length
        PayloadBytes = (Get-ChildItem -LiteralPath $payload -File -Recurse | Measure-Object Length -Sum).Sum
        Archive = $archive
        ArchiveBytes = (Get-Item -LiteralPath $archive).Length
        ArchiveSha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
        Installer = $installer
        InstallerBytes = if ($installer) { (Get-Item -LiteralPath $installer).Length }
        InstallerSha256 = if ($installer) { (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash }
    }
} finally {
    $stagePath = (Resolve-Path -LiteralPath $stage).Path
    $buildPath = (Resolve-Path -LiteralPath $buildRoot).Path
    if ((Split-Path -Parent $stagePath) -eq $buildPath -and (Split-Path -Leaf $stagePath) -like 'package-*') {
        Remove-Item -LiteralPath $stagePath -Recurse -Force
    }
}
