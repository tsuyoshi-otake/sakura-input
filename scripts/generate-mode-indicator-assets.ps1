[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$stopwatch = [Diagnostics.Stopwatch]::StartNew()

Add-Type -AssemblyName System.Drawing

$outputDirectory = Join-Path $PSScriptRoot '..\crates\sakura-tsf\assets\mode-indicator'
[System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null

$modes = @(
    [pscustomobject]@{ Name = 'hiragana'; Glyph = 'あ'; Rect16 = @(2, 2, 11, 12); Rect32 = @(5, 4, 21, 24); Slashed = $false },
    [pscustomobject]@{ Name = 'katakana'; Glyph = 'ア'; Rect16 = @(2, 2, 11, 12); Rect32 = @(5, 4, 21, 24); Slashed = $false },
    [pscustomobject]@{ Name = 'half-katakana'; Glyph = 'ｱ'; Rect16 = @(3, 2, 10, 12); Rect32 = @(6, 4, 20, 24); Slashed = $false },
    [pscustomobject]@{ Name = 'full-alnum'; Glyph = 'Ａ'; Rect16 = @(2, 3, 12, 11); Rect32 = @(4, 6, 24, 21); Slashed = $false },
    [pscustomobject]@{ Name = 'half-alnum'; Glyph = 'A'; Rect16 = @(4, 3, 8, 11); Rect32 = @(8, 6, 16, 21); Slashed = $false },
    [pscustomobject]@{ Name = 'direct'; Glyph = 'A'; Rect16 = @(2, 2, 12, 12); Rect32 = @(4, 4, 24, 24); Slashed = $true }
)

$themes = @(
    [pscustomobject]@{ Name = 'dark'; Red = 255; Green = 255; Blue = 255 },
    [pscustomobject]@{ Name = 'light'; Red = 28; Green = 28; Blue = 28 }
)

function Get-AlphaMask {
    param(
        [Parameter(Mandatory)] [string] $Glyph,
        [Parameter(Mandatory)] [int] $Size,
        [Parameter(Mandatory)] [int[]] $TargetRect,
        [Parameter(Mandatory)] [bool] $Slashed
    )

    $source = [System.Drawing.Bitmap]::new(256, 256, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $sourceGraphics = [System.Drawing.Graphics]::FromImage($source)
    $font = [System.Drawing.Font]::new(
        'Yu Gothic UI Semibold',
        180,
        [System.Drawing.FontStyle]::Regular,
        [System.Drawing.GraphicsUnit]::Pixel
    )
    $format = [System.Drawing.StringFormat]::GenericTypographic
    try {
        $sourceGraphics.Clear([System.Drawing.Color]::Transparent)
        $sourceGraphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
        $sourceGraphics.DrawString(
            $Glyph,
            $font,
            [System.Drawing.Brushes]::White,
            [System.Drawing.PointF]::new(0, 0),
            $format
        )

        $left = $source.Width
        $top = $source.Height
        $right = -1
        $bottom = -1
        for ($y = 0; $y -lt $source.Height; $y++) {
            for ($x = 0; $x -lt $source.Width; $x++) {
                if ($source.GetPixel($x, $y).A -eq 0) { continue }
                if ($x -lt $left) { $left = $x }
                if ($x -gt $right) { $right = $x }
                if ($y -lt $top) { $top = $y }
                if ($y -gt $bottom) { $bottom = $y }
            }
        }
        if ($right -lt $left -or $bottom -lt $top) {
            throw "Yu Gothic UI Semibold produced an empty glyph for '$Glyph'."
        }

        $target = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $targetGraphics = [System.Drawing.Graphics]::FromImage($target)
        try {
            $targetGraphics.Clear([System.Drawing.Color]::Transparent)
            $targetGraphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
            $targetGraphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $targetGraphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $targetGraphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $destination = [System.Drawing.Rectangle]::new(
                $TargetRect[0], $TargetRect[1], $TargetRect[2], $TargetRect[3]
            )
            $targetGraphics.DrawImage(
                $source,
                $destination,
                $left,
                $top,
                $right - $left + 1,
                $bottom - $top + 1,
                [System.Drawing.GraphicsUnit]::Pixel
            )

            if ($Slashed) {
                $targetGraphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
                $penWidth = if ($Size -eq 16) { 1.5 } else { 3.0 }
                $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, $penWidth)
                try {
                    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
                    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
                    $inset = if ($Size -eq 16) { 2.5 } else { 5.0 }
                    $targetGraphics.DrawLine($pen, $inset, $Size - $inset, $Size - $inset, $inset)
                } finally {
                    $pen.Dispose()
                }
            }

            $alpha = [byte[]]::new($Size * $Size)
            for ($y = 0; $y -lt $Size; $y++) {
                for ($x = 0; $x -lt $Size; $x++) {
                    $alpha[$y * $Size + $x] = $target.GetPixel($x, $y).A
                }
            }
            return $alpha
        } finally {
            $targetGraphics.Dispose()
            $target.Dispose()
        }
    } finally {
        $font.Dispose()
        $sourceGraphics.Dispose()
        $source.Dispose()
    }
}

foreach ($mode in $modes) {
    foreach ($size in @(16, 32)) {
        $rect = if ($size -eq 16) { $mode.Rect16 } else { $mode.Rect32 }
        $alpha = Get-AlphaMask -Glyph $mode.Glyph -Size $size -TargetRect $rect -Slashed $mode.Slashed
        foreach ($theme in $themes) {
            $pixels = [byte[]]::new($size * $size * 4)
            for ($index = 0; $index -lt $alpha.Length; $index++) {
                $a = [int] $alpha[$index]
                $offset = $index * 4
                # CreateIconIndirect expects premultiplied BGRA for a 32-bit alpha icon.
                $pixels[$offset] = [byte] [Math]::Round($theme.Blue * $a / 255.0)
                $pixels[$offset + 1] = [byte] [Math]::Round($theme.Green * $a / 255.0)
                $pixels[$offset + 2] = [byte] [Math]::Round($theme.Red * $a / 255.0)
                $pixels[$offset + 3] = [byte] $a
            }
            $path = Join-Path $outputDirectory ("{0}-{1}-{2}.bgra" -f $mode.Name, $size, $theme.Name)
            [System.IO.File]::WriteAllBytes($path, $pixels)
        }
    }
}

$stopwatch.Stop()
Write-Host ("Generated {0} original mode-indicator assets in {1:N2}s." -f ($modes.Count * 4), $stopwatch.Elapsed.TotalSeconds)
