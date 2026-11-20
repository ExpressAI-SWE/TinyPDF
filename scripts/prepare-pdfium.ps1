param(
    [Parameter(Mandatory)]
    [ValidateSet('win-x64', 'win-arm64', 'linux-x64', 'linux-arm64', 'mac-x64', 'mac-arm64')]
    [string] $Platform,

    [string] $Archive
)

$ErrorActionPreference = 'Stop'
$release = 'chromium%2F8152'
$checksums = @{
    'win-x64'     = 'cee2b0e9990bc67c0721774932961b9dbf2766046c10eec925acd4cfba6fdfe3'
    'win-arm64'   = '8614b10c6a780acc3ae9fdd425d6b61f9d22ae84301b19e1ac82f573930c217c'
    'linux-x64'   = '384ab5c51882408a143a21f9c14dcd2caf1f75280ae41c554d0489b46c3dba9f'
    'linux-arm64' = 'c6ed1a91234aede0cfafdc09c401b83bd44ac69deafc4599fda75d511c36403e'
    'mac-x64'     = 'acb975e637ffa1f41430205c62baac70e34b60811e22fc0e0274dfcd601fcc'
    'mac-arm64'   = '30b0c3f55448840f0947b54bedbe35185bbaccbadd35b8cd960270d6fa6caf80'
}
$root = Split-Path -Parent $PSScriptRoot
$buildRoot = Join-Path $root 'target'
$name = "pdfium-$Platform.tgz"
$expectedHash = $checksums[$Platform]

if (-not $Archive) {
    $download = Join-Path $buildRoot 'pdfium-download'
    New-Item -ItemType Directory -Path $download -Force | Out-Null
    $Archive = Join-Path $download $name
    if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
        $partial = "$Archive.$([guid]::NewGuid()).partial"
        try {
            $uri = "https://github.com/bblanchon/pdfium-binaries/releases/download/$release/$name"
            Invoke-WebRequest -Uri $uri -OutFile $partial -TimeoutSec 120
            if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash -ne $expectedHash) {
                throw "SHA-256 mismatch for $name."
            }
            [System.IO.File]::Move($partial, $Archive)
        } finally {
            if (Test-Path -LiteralPath $partial -PathType Leaf) {
                Remove-Item -LiteralPath $partial
            }
        }
    }
}

$Archive = (Resolve-Path -LiteralPath $Archive).Path
if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash -ne $expectedHash) {
    throw "SHA-256 mismatch for $Archive. Obtain the pinned $name release asset."
}

$entry = switch -Wildcard ($Platform) {
    'win-*' { 'bin/pdfium.dll' }
    'linux-*' { 'lib/libpdfium.so' }
    'mac-*' { 'lib/libpdfium.dylib' }
}
$out = Join-Path $buildRoot "pdfium/$Platform"
New-Item -ItemType Directory -Path $out -Force | Out-Null
& tar -xzf $Archive -C $out $entry LICENSE VERSION licenses
if ($LASTEXITCODE -ne 0) {
    throw "Could not extract the runtime and licenses from $Archive."
}

(Get-Item -LiteralPath (Join-Path $out $entry)).FullName
