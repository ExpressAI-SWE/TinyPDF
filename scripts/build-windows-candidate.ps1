param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
$tools = (New-Item -ItemType Directory -Path './target/windows-packaging' -Force).FullName
function Fetch-Archive([string] $Uri, [string] $Name, [string] $Hash) {
    $archive = Join-Path $tools $Name
    if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
        $partial = "$archive.$([guid]::NewGuid()).partial"
        try {
            Invoke-WebRequest -Uri $Uri -OutFile $partial -TimeoutSec 120
            if ((Get-FileHash -LiteralPath $partial).Hash -ne $Hash) { throw "$Name failed integrity verification." }
            [IO.File]::Move($partial, $archive)
        } finally {
            if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial }
        }
    }
    if ((Get-FileHash -LiteralPath $archive).Hash -ne $Hash) { throw "$Name failed integrity verification." }
    return $archive
}

$nsis = Fetch-Archive 'https://community.chocolatey.org/api/v2/package/nsis.portable/3.13.0' 'nsis-portable.zip' `
    'C3597A5BBB40521BAD9E762B3175B8BCDE4768FB8A51376471A3D8293A2E86F2'
$wix = Fetch-Archive 'https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip' 'wix314.zip' `
    '6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31'
$nsisStage = Join-Path $tools 'nsis-package'
$nsisDirectory = Join-Path $tools 'nsis'
$wixDirectory = Join-Path $tools 'wix'
Expand-Archive -LiteralPath $nsis -DestinationPath $nsisStage -Force
Expand-Archive -LiteralPath (Join-Path $nsisStage 'tools/nsis-3.13.zip') -DestinationPath $nsisDirectory -Force
Expand-Archive -LiteralPath $wix -DestinationPath $wixDirectory -Force
$makeNsi = Get-ChildItem -LiteralPath $nsisDirectory -Recurse -Filter 'makensis.exe' |
    Sort-Object { $_.FullName.Length } | Select-Object -First 1 -ExpandProperty FullName
if (-not $makeNsi) { throw 'Could not find makensis in the pinned archive.' }
$sdk = Get-ChildItem -Directory "${env:ProgramFiles(x86)}/Windows Kits/10/bin" |
    Where-Object { $_.Name -match '^10\.' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'x64/makeappx.exe')) } |
    Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1 |
    ForEach-Object { Join-Path $_.FullName 'x64' }
if (-not $sdk) { throw 'The Windows SDK MakeAppx tool was not found.' }
$sdk | Set-Content -LiteralPath (Join-Path $tools 'sdk.txt')

& (Join-Path $PSScriptRoot 'package.ps1') -Target x86_64-pc-windows-msvc -Binary $Binary -MakeNsi $makeNsi -OutputDirectory $OutputDirectory
$portable = Join-Path $OutputDirectory 'TinyPDF-win-x64.zip'
& (Join-Path $PSScriptRoot 'package-msi.ps1') -PortableZip $portable -WixBin $wixDirectory
& (Join-Path $PSScriptRoot 'package-msix.ps1') -PortableZip $portable -MakeAppx (Join-Path $sdk 'makeappx.exe') -Standalone
$candidate = (Resolve-Path -LiteralPath $OutputDirectory).Path
$checksums = foreach ($file in Get-ChildItem -LiteralPath $candidate -File) {
    "$((Get-FileHash -LiteralPath $file.FullName).Hash.ToLowerInvariant())  $($file.Name)"
}
$checksums | Set-Content -LiteralPath (Join-Path $candidate 'SHA256SUMS.txt') -Encoding ascii
