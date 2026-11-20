param(
    [Parameter(Mandatory)] [string] $PackageDirectory,
    [Parameter(Mandatory)] [string] $Pdf,
    [Parameter(Mandatory)] [ValidateSet('NSIS', 'MSI', 'MSIX')] [string] $Installer,
    [string] $SignTool,
    [string] $Logs = './target/installer-test'
)

$ErrorActionPreference = 'Stop'
$packageDirectory = (Resolve-Path -LiteralPath $PackageDirectory).Path
$pdf = (Resolve-Path -LiteralPath $Pdf).Path
$logs = (New-Item -ItemType Directory -Path $Logs -Force).FullName
$registered = 'HKCU:/Software/RegisteredApplications'
$programs = [Environment]::GetFolderPath('Programs')

function Invoke-InstallerProcess([string] $Executable, [string] $Arguments) {
    $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -PassThru -WindowStyle Hidden
    try {
        if (-not $process.WaitForExit(120000)) {
            $process.Kill()
            throw "$Executable did not exit within two minutes."
        }
        if ($process.ExitCode -ne 0) { throw "$Executable exited with code $($process.ExitCode)." }
    } finally {
        $process.Dispose()
    }
}

function Test-Reader([string] $Directory) {
    $reader = Join-Path $Directory 'tinypdf.exe'
    $version = (Get-Item -LiteralPath $reader).VersionInfo.ProductVersion
    if (-not $version) { throw 'The installed executable lacks version metadata.' }
    & (Join-Path $PSScriptRoot 'bench-pdfium.ps1') -Executable $reader -Pdf $pdf -Runs 3 |
        Set-Content -LiteralPath (Join-Path $logs "$Installer-reader.json")
}

function Test-Association([string] $Name, [string] $ProgId, [string] $Directory) {
    $location = Get-ItemPropertyValue -LiteralPath $registered -Name $Name
    $association = Get-ItemPropertyValue -LiteralPath "HKCU:/$location/FileAssociations" -Name '.pdf'
    if ($association -ne $ProgId) { throw "Default Apps registration does not reference $ProgId." }
    $key = Get-Item -LiteralPath "HKCU:/Software/Classes/$ProgId/shell/open/command"
    $expected = '"' + (Join-Path $Directory 'tinypdf.exe') + '" "%1"'
    if ($key.GetValue('') -ne $expected) { throw 'The PDF association command has an unexpected target.' }
    $shortcut = Join-Path $programs "$Name.lnk"
    if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) { throw 'Start Menu shortcut is missing.' }
    $target = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath
    if ($target -ne (Join-Path $Directory 'tinypdf.exe')) { throw 'Start Menu shortcut has an unexpected target.' }
}

function Assert-Clean([string] $Name, [string] $Directory, [string] $ProgId) {
    $registration = if (Test-Path -LiteralPath $registered) {
        (Get-Item -LiteralPath $registered).GetValue($Name)
    }
    if ((Test-Path -LiteralPath $Directory) -or
        (Test-Path -LiteralPath (Join-Path $programs "$Name.lnk")) -or
        (Test-Path -LiteralPath "HKCU:/Software/Classes/$ProgId") -or
        $registration) {
        throw "$Name has existing files or registry entries. Use an isolated test account."
    }
}

switch ($Installer) {
    'NSIS' {
        $directory = Join-Path $env:LOCALAPPDATA 'Programs/TinyPDF'
        $arp = 'HKCU:/Software/Microsoft/Windows/CurrentVersion/Uninstall/TinyPDF'
        Assert-Clean 'TinyPDF' $directory 'TinyPDF.PDF'
        if (Test-Path -LiteralPath $arp) { throw 'An existing TinyPDF setup is registered.' }
        $uninstaller = Join-Path $directory 'uninstall.exe'
        try {
            Invoke-InstallerProcess (Join-Path $packageDirectory 'TinyPDF-Setup.exe') '/S'
            Test-Association 'TinyPDF' 'TinyPDF.PDF' $directory
            Test-Reader $directory
        } finally {
            if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
                Invoke-InstallerProcess $uninstaller '/S'
                $deadline = [DateTime]::UtcNow.AddSeconds(20)
                while ((Test-Path -LiteralPath $directory) -and [DateTime]::UtcNow -lt $deadline) {
                    Start-Sleep -Milliseconds 250
                }
            }
        }
        Assert-Clean 'TinyPDF' $directory 'TinyPDF.PDF'
        if (Test-Path -LiteralPath $arp) { throw 'NSIS uninstall registration was left behind.' }
    }
    'MSI' {
        $directory = Join-Path $env:LOCALAPPDATA 'TinyPDF MSI'
        Assert-Clean 'TinyPDF (MSI)' $directory 'TinyPDF.MSI.PDF'
        $msi = Join-Path $packageDirectory 'TinyPDF.msi'
        $msiexec = Join-Path $env:WINDIR 'System32/msiexec.exe'
        Invoke-InstallerProcess $msiexec "/i `"$msi`" /qn /norestart /L*v `"$logs/msi-install.log`""
        try {
            Test-Association 'TinyPDF (MSI)' 'TinyPDF.MSI.PDF' $directory
            Test-Reader $directory
        } finally {
            Invoke-InstallerProcess $msiexec "/x `"$msi`" /qn /norestart /L*v `"$logs/msi-uninstall.log`""
        }
        Assert-Clean 'TinyPDF (MSI)' $directory 'TinyPDF.MSI.PDF'
    }
    'MSIX' {
        if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $SignTool) {
            throw 'MSIX test signing requires an ephemeral GitHub-hosted Windows runner and SignTool.'
        }
        Import-Module Appx -UseWindowsPowerShell
        if (Get-AppxPackage -Name 'TinyPDF.Reader') { throw 'An existing TinyPDF MSIX is registered.' }
        $copy = Join-Path $logs 'TinyPDF-test.msix'
        $certificate = $null
        $cer = Join-Path $logs 'test-publisher.cer'
        try {
            Copy-Item -LiteralPath (Join-Path $packageDirectory 'TinyPDF.msix') -Destination $copy
            $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=TinyPDF' `
                -CertStoreLocation 'Cert:/CurrentUser/My' -NotAfter ([DateTime]::Now.AddHours(6))
            Export-Certificate -Cert $certificate -FilePath $cer | Out-Null
            Import-Certificate -FilePath $cer -CertStoreLocation 'Cert:/LocalMachine/TrustedPeople' | Out-Null
            & $SignTool sign /fd SHA256 /sha1 $certificate.Thumbprint $copy
            if ($LASTEXITCODE) { throw 'Failed to sign the disposable MSIX copy.' }
            & $SignTool verify /pa $copy
            if ($LASTEXITCODE) { throw 'The disposable MSIX signature did not verify.' }
            Add-AppxPackage -Path $copy
            $app = Get-AppxPackage -Name 'TinyPDF.Reader'
            if ($app.Architecture -ne 'X64') { throw 'The x64 package did not register.' }
            Test-Reader $app.InstallLocation
        } finally {
            Get-AppxPackage -Name 'TinyPDF.Reader' | ForEach-Object { Remove-AppxPackage -Package $_.PackageFullName }
            if ($certificate) {
                foreach ($store in 'Cert:/LocalMachine/TrustedPeople', 'Cert:/CurrentUser/My') {
                    $entry = Join-Path $store $certificate.Thumbprint
                    if (Test-Path -LiteralPath $entry) { Remove-Item -LiteralPath $entry }
                }
            }
            foreach ($file in $copy, $cer) {
                if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file }
            }
        }
        if (Get-AppxPackage -Name 'TinyPDF.Reader') { throw 'MSIX removal left registration behind.' }
    }
}
"PASS $Installer install, reader smoke, and removal"
