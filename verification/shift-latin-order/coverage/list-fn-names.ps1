$ErrorActionPreference = 'Stop'
$path = Join-Path $PSScriptRoot 'llvm-cov.json'
$json = Get-Content -Raw -Path $path | ConvertFrom-Json
$file = $json.data[0].files | Where-Object { $_.filename -match 'dispatch\.rs$' } | Select-Object -First 1
$file.functions | ForEach-Object { $_.name } | Select-String -Pattern 'feed_character|apply_backspace|render_preedit|resync_shifted|keymap'
