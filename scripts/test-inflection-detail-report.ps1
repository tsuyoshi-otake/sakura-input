Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'inflection-detail-report.ps1')

function Assert-ValidReport {
    param([Parameter(Mandatory)][string]$Json, [Parameter(Mandatory)][long]$Expected)
    $report = ConvertFrom-Json -InputObject $Json
    $actual = Get-InflectionDetailEmittedCount $report
    if ($actual -ne $Expected) { throw "expected emitted count $Expected, got $actual" }
}

function Assert-InvalidReport {
    param([Parameter(Mandatory)][string]$Json)
    $report = ConvertFrom-Json -InputObject $Json
    try {
        Get-InflectionDetailEmittedCount $report | Out-Null
    }
    catch { return }
    throw 'expected invalid inflection detail accounting to be rejected'
}

Assert-ValidReport '{"schema_version":1}' 0
Assert-ValidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":3,"emitted":1,"ambiguous":1,"unmapped":1,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}' 1
Assert-ValidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":1,"emitted":1,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0,"native":{"native_entries":3,"considered":2,"emitted":1,"ambiguous":1,"unmapped":1,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}}' 2
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":0,"emitted":0,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0,"native":{"native_entries":3,"considered":2,"emitted":1,"ambiguous":0,"unmapped":1,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":1.0,"emitted":1,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":1,"emitted":true,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":1,"emitted":0,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0,"timestamp":"2026-10-28T00:00:00Z"}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":1,"emitted":0,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":18446744073709551615,"emitted":0,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":9223372036854775807,"emitted":9223372036854775807,"ambiguous":1,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}'
Assert-InvalidReport '{"inflection_detail_import":{"schema_version":"sakura.inflection-detail-import.v1","generated":"2026-10-28T00:00:00Z","emitted":0,"ambiguous":0,"unmapped":0,"final_changed":0,"existing":0,"missing_lemma":0,"missing_lemma_detail":0}}'
try {
    Get-CheckedDetailCountSum @([long]::MaxValue, 1L) | Out-Null
}
catch { 'Inflection detail report validation tests passed.'; exit 0 }
throw 'expected final dictionary detail total overflow to be rejected'
