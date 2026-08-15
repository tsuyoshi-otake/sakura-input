$ErrorActionPreference = 'Stop'
$path = Join-Path $PSScriptRoot 'llvm-cov.json'
$json = Get-Content -Raw -Path $path | ConvertFrom-Json
$wanted = @(
    'feed_character',
    'apply_backspace',
    'render_preedit',
    'resync_shifted_ascii_from_raw'
)
$files = $json.data[0].files | Where-Object { $_.filename -match 'dispatch\.rs$' }
foreach ($file in $files) {
    Write-Output ("FILE {0}" -f $file.filename)
    foreach ($fn in $file.functions) {
        $name = [string]$fn.name
        foreach ($needle in $wanted) {
            if ($name -like ("*{0}*" -f $needle)) {
                $regions = @($fn.regions)
                $covered = @($regions | Where-Object { $_[4] -gt 0 }).Count
                Write-Output ("FN {0} count={1} regions={2} covered_regions={3}" -f $name, $fn.count, $regions.Count, $covered)
            }
        }
    }
    if ($file.summary) {
        Write-Output ("FILE_SUMMARY lines={0}/{1} regions={2}/{3} functions={4}/{5}" -f `
            $file.summary.lines.covered, $file.summary.lines.count, `
            $file.summary.regions.covered, $file.summary.regions.count, `
            $file.summary.functions.covered, $file.summary.functions.count)
    }
}
