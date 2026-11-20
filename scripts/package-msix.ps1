param(
    [Parameter(Mandatory)] [string] $PortableZip,
    [Parameter(Mandatory)] [string] $MakeAppx,
    [string] $Publisher = 'CN=TinyPDF',
    [string] $SignTool,
    [string] $CertificateThumbprint,
    [switch] $Standalone
)

$ErrorActionPreference = 'Stop'
if ([bool]$SignTool -ne [bool]$CertificateThumbprint) {
    throw 'Supply both SignTool and CertificateThumbprint to sign the bundle.'
}
$PortableZip = (Resolve-Path -LiteralPath $PortableZip).Path
$dist = Split-Path -Parent $PortableZip
$work = [IO.Directory]::CreateTempSubdirectory('TinyPDF-msix-').FullName
try {
    Expand-Archive -LiteralPath $PortableZip -DestinationPath $work
    $payload = Join-Path $work 'TinyPDF-win-x64'
    $info = (Get-Item -LiteralPath (Join-Path $payload 'tinypdf.exe')).VersionInfo
    if (-not $info.ProductVersion) { throw 'The Windows executable lacks version information.' }
    $version = "$($info.ProductMajorPart).$($info.ProductMinorPart).$($info.ProductBuildPart)"
    $packages = (New-Item -ItemType Directory -Path (Join-Path $work 'packages')).FullName
    $assets = (New-Item -ItemType Directory -Path (Join-Path $payload 'Assets')).FullName
    Add-Type -AssemblyName System.Drawing
    $icon = [Drawing.Image]::FromFile((Join-Path $PSScriptRoot '../assets/tinypdf.png'))
    try {
        foreach ($size in 44, 50, 150) {
            $bitmap = [Drawing.Bitmap]::new($icon, $size, $size)
            try {
                $bitmap.Save((Join-Path $assets "Logo$size.png"), [Drawing.Imaging.ImageFormat]::Png)
            } finally {
                $bitmap.Dispose()
            }
        }
    } finally {
        $icon.Dispose()
    }
    $publisherXml = [Security.SecurityElement]::Escape($Publisher)
    @"
<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap rescap">
  <Identity Name="TinyPDF.Reader" Publisher="$publisherXml" Version="$version.0" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>TinyPDF</DisplayName>
    <PublisherDisplayName>TinyPDF</PublisherDisplayName>
    <Logo>Assets\Logo50.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources><Resource Language="en-us" /></Resources>
  <Applications>
    <Application Id="Reader" Executable="tinypdf.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="TinyPDF" Description="TinyPDF reader" BackgroundColor="transparent"
          Square44x44Logo="Assets\Logo44.png" Square150x150Logo="Assets\Logo150.png" />
      <Extensions>
        <uap:Extension Category="windows.fileTypeAssociation">
          <uap:FileTypeAssociation Name="pdf">
            <uap:SupportedFileTypes><uap:FileType>.pdf</uap:FileType></uap:SupportedFileTypes>
          </uap:FileTypeAssociation>
        </uap:Extension>
      </Extensions>
    </Application>
  </Applications>
  <Capabilities><rescap:Capability Name="runFullTrust" /></Capabilities>
</Package>
"@ | Set-Content -Encoding utf8 -LiteralPath (Join-Path $payload 'AppxManifest.xml')
    $package = Join-Path $packages $(if ($Standalone) { 'TinyPDF.msix' } else { 'TinyPDF-x64.msix' })
    & $MakeAppx pack /d $payload /p $package
    if ($LASTEXITCODE) { throw 'MakeAppx could not build the package.' }
    if (-not $Standalone) {
        $package = Join-Path $work 'TinyPDF-win-x64.msixbundle'
        & $MakeAppx bundle /d $packages /p $package
        if ($LASTEXITCODE) { throw 'MakeAppx could not build the bundle.' }
    }
    if ($SignTool) {
        & $SignTool sign /sha1 $CertificateThumbprint /fd SHA256 $package
        if ($LASTEXITCODE) { throw 'SignTool could not sign the package.' }
    }
    $output = Join-Path $dist (Split-Path -Leaf $package)
    [IO.File]::Move($package, $output)
    Get-FileHash -LiteralPath $output
} finally {
    if ((Split-Path -Parent $work) -eq [IO.Path]::GetTempPath().TrimEnd('\') -and
        (Split-Path -Leaf $work) -like 'TinyPDF-msix-*') {
        Remove-Item -LiteralPath $work -Recurse -Force
    }
}
