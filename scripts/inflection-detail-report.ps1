function Get-InflectionDetailEmittedCount {
    param([Parameter(Mandatory)][object]$DictionaryReport)

    $importProperty = $DictionaryReport.PSObject.Properties['inflection_detail_import']
    if ($null -eq $importProperty) { return 0L }
    $import = $importProperty.Value
    if ($null -eq $import) { throw 'inflection detail import accounting is missing or invalid' }
    if ($import.schema_version -cne 'sakura.inflection-detail-import.v1') {
        throw 'inflection detail import accounting is missing or invalid'
    }

    $fields = @('generated', 'emitted', 'ambiguous', 'unmapped', 'final_changed', 'existing', 'missing_lemma', 'missing_lemma_detail')
    $counts = @{}
    foreach ($field in $fields) {
        $property = $import.PSObject.Properties[$field]
        if ($null -eq $property -or $null -eq $property.Value) {
            throw "inflection detail import $field count is missing or invalid"
        }
        $value = $property.Value
        $typeCode = [Type]::GetTypeCode($value.GetType())
        if ($typeCode -notin @(
                [TypeCode]::SByte, [TypeCode]::Byte, [TypeCode]::Int16, [TypeCode]::UInt16,
                [TypeCode]::Int32, [TypeCode]::UInt32, [TypeCode]::Int64, [TypeCode]::UInt64
            )) {
            throw "inflection detail import $field count must be an integer"
        }
        if ([decimal]$value -lt 0 -or [decimal]$value -gt [decimal][long]::MaxValue) {
            throw "inflection detail import $field count is outside the supported range"
        }
        $counts[$field] = [long]$value
    }

    $sum = [long]$counts.emitted
    foreach ($field in @('ambiguous', 'unmapped', 'final_changed', 'existing', 'missing_lemma', 'missing_lemma_detail')) {
        $count = [long]$counts[$field]
        if ($count -gt ([long]::MaxValue - $sum)) {
            throw 'inflection detail import accounting overflows the supported range'
        }
        $sum += $count
    }
    if ([long]$counts.generated -ne $sum) {
        throw 'inflection detail import accounting is missing or invalid'
    }
    $total = [long]$counts.emitted
    $nativeProperty = $import.PSObject.Properties['native']
    if ($null -ne $nativeProperty) {
        $native = $nativeProperty.Value
        if ($null -eq $native) { throw 'native inflection accounting is missing or invalid' }
        $nativeCounts = @{}
        foreach ($field in @('native_entries', 'considered', 'emitted', 'ambiguous', 'unmapped', 'final_changed', 'existing', 'missing_lemma', 'missing_lemma_detail')) {
            $property = $native.PSObject.Properties[$field]
            if ($null -eq $property -or $null -eq $property.Value) { throw "native inflection $field count is missing or invalid" }
            $value = $property.Value
            $typeCode = [Type]::GetTypeCode($value.GetType())
            if ($typeCode -notin @([TypeCode]::SByte, [TypeCode]::Byte, [TypeCode]::Int16, [TypeCode]::UInt16, [TypeCode]::Int32, [TypeCode]::UInt32, [TypeCode]::Int64, [TypeCode]::UInt64) -or
                [decimal]$value -lt 0 -or [decimal]$value -gt [decimal][long]::MaxValue) {
                throw "native inflection $field count must be a nonnegative integer"
            }
            $nativeCounts[$field] = [long]$value
        }
        $considered = Get-CheckedDetailCountSum @($nativeCounts.emitted, $nativeCounts.ambiguous, $nativeCounts.final_changed, $nativeCounts.existing, $nativeCounts.missing_lemma, $nativeCounts.missing_lemma_detail)
        $nativeTotal = Get-CheckedDetailCountSum @($nativeCounts.considered, $nativeCounts.unmapped)
        if ($considered -ne $nativeCounts.considered -or $nativeTotal -ne $nativeCounts.native_entries) {
            throw 'native inflection detail accounting is missing or invalid'
        }
        $total = Get-CheckedDetailCountSum @($total, $nativeCounts.emitted)
    }
    return $total
}

function Get-CheckedDetailCountSum {
    param([Parameter(Mandatory)][long[]]$Counts)

    $sum = 0L
    foreach ($count in $Counts) {
        if ($count -lt 0 -or $count -gt ([long]::MaxValue - $sum)) {
            throw 'dictionary detail count total is outside the supported range'
        }
        $sum += $count
    }
    return $sum
}
