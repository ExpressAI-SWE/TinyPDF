param(
    [Parameter(Mandatory)] [string] $PortableZip,
    [Parameter(Mandatory)] [string] $WixBin
)

$ErrorActionPreference = 'Stop'
$PortableZip = (Resolve-Path -LiteralPath $PortableZip).Path
$WixBin = (Resolve-Path -LiteralPath $WixBin).Path
$output = Join-Path (Split-Path -Parent $PortableZip) 'TinyPDF.msi'
if (Test-Path -LiteralPath $output) { throw "Output already exists: $output" }
$work = [IO.Directory]::CreateTempSubdirectory('TinyPDF-msi-').FullName
try {
    Expand-Archive -LiteralPath $PortableZip -DestinationPath $work
    $payload = Join-Path $work 'TinyPDF-win-x64'
    $info = (Get-Item -LiteralPath (Join-Path $payload 'tinypdf.exe')).VersionInfo
    if (-not $info.ProductVersion) { throw 'The Windows executable lacks version information.' }
    $version = "$($info.ProductMajorPart).$($info.ProductMinorPart).$($info.ProductBuildPart)"

    function New-NoticeFiles([string] $Folder) {
        foreach ($file in (Get-ChildItem -LiteralPath $Folder -File | Sort-Object Name)) {
            $escapedPath = [Security.SecurityElement]::Escape($file.FullName)
            $escapedName = [Security.SecurityElement]::Escape($file.Name)
            "<File Name=`"$escapedName`" Source=`"$escapedPath`" />"
        }
    }

    $noticesPath = Join-Path $work 'notices.wxi'
    $noticesDir = Join-Path $payload 'PDFium'
    $licensesDir = Join-Path $noticesDir 'licenses'
    @"
<Include xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Directory Id="PdfiumNoticesDir" Name="PDFium">
    <Component Id="PdfiumNotices" Guid="70788A5D-71EA-4411-B10B-3CFF68678D80" Win64="yes">
      $(New-NoticeFiles $noticesDir)
      <RemoveFolder Id="RemoveNotices" On="uninstall" />
      <RegistryValue Root="HKCU" Key="Software\ExpressAI-SWE\TinyPDF MSI" Name="PdfiumNotices"
                     Type="integer" Value="1" KeyPath="yes" />
    </Component>
    <Directory Id="PdfiumLicensesDir" Name="licenses">
      <Component Id="PdfiumLicenses" Guid="16F28267-87C3-4DA6-871C-C1CE3C9BC9B1" Win64="yes">
        $(New-NoticeFiles $licensesDir)
        <RemoveFolder Id="RemoveLicenses" On="uninstall" />
        <RegistryValue Root="HKCU" Key="Software\ExpressAI-SWE\TinyPDF MSI" Name="PdfiumLicenses"
                       Type="integer" Value="1" KeyPath="yes" />
      </Component>
    </Directory>
  </Directory>
</Include>
"@ | Set-Content -Encoding utf8 -LiteralPath $noticesPath
    $icon = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../assets/tinypdf.ico')).Path
    $object = Join-Path $work 'windows-install.wixobj'
    & (Join-Path $WixBin 'candle.exe') -nologo -wx -arch x64 "-dVersion=$version" "-dPayload=$payload" "-dNotices=$noticesPath" "-dIconPath=$icon" -out $object (Join-Path $PSScriptRoot 'windows-install.wxs')
    if ($LASTEXITCODE) { throw 'WiX could not compile the MSI recipe.' }
    $staged = Join-Path $work 'TinyPDF.msi'
    # ICE91 describes per-machine deployment of per-user paths, which this MSI disallows.
    & (Join-Path $WixBin 'light.exe') -nologo -wx -sice:ICE91 -out $staged $object
    if ($LASTEXITCODE) { throw 'WiX could not build and validate the MSI.' }
    [IO.File]::Move($staged, $output)
    Get-FileHash -LiteralPath $output
} finally {
    $workPath = (Resolve-Path -LiteralPath $work).Path
    if ((Split-Path -Parent $workPath) -eq [IO.Path]::GetTempPath().TrimEnd('\') -and
        (Split-Path -Leaf $workPath) -like 'TinyPDF-msi-*') {
        Remove-Item -LiteralPath $workPath -Recurse -Force
    }
}
