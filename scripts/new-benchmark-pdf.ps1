param(
    [Parameter(Mandatory)]
    [string] $Output,

    [ValidateRange(1, 10000)]
    [int] $Pages = 501
)

$ErrorActionPreference = 'Stop'
$pdf = [System.Text.StringBuilder]::new()
$offsets = [System.Collections.Generic.List[int]]::new()

function Add-PdfObject([string] $Body) {
    $offsets.Add($pdf.Length)
    $null = $pdf.Append("$($offsets.Count) 0 obj`n$Body`nendobj`n")
}

$null = $pdf.Append("%PDF-1.4`n")
Add-PdfObject '<< /Type /Catalog /Pages 2 0 R >>'
$kids = (0..($Pages - 1) | ForEach-Object { "$($_ * 2 + 4) 0 R" }) -join ' '
Add-PdfObject "<< /Type /Pages /Count $Pages /Kids [$kids] >>"
Add-PdfObject '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'
for ($page = 1; $page -le $Pages; $page++) {
    $contents = $page * 2 + 3
    Add-PdfObject "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents $contents 0 R >>"
    $lines = "BT /F1 22 Tf 72 720 Td (TinyPDF benchmark fixture, page $page of $Pages) Tj ET`n"
    $lines += "BT /F1 11 Tf 72 690 Td (Search target: native document reader) Tj ET`n"
    Add-PdfObject "<< /Length $($lines.Length) >>`nstream`n${lines}endstream"
}
$xref = $pdf.Length
$count = $offsets.Count + 1
$null = $pdf.Append("xref`n0 $count`n0000000000 65535 f `n")
foreach ($offset in $offsets) {
    $null = $pdf.Append("$($offset.ToString('D10')) 00000 n `n")
}
$null = $pdf.Append("trailer`n<< /Size $count /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")

$path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
$stream = [System.IO.File]::Open($path, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
try {
    $stream.Write([System.Text.Encoding]::ASCII.GetBytes($pdf.ToString()))
} finally {
    $stream.Dispose()
}
$path
