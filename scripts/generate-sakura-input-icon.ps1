param(
    [string]$SourcePath = (Join-Path $PSScriptRoot '..\..\sakura-editor-next\src\main\resources\images\sakura_editor_next.png'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\assets\sakura-input-icon'),
    [double]$HueShiftDegrees = 65
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [IO.Path]::GetFullPath($SourcePath)
$outputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$outputPng = Join-Path $outputDirectory 'sakura-input.png'
$outputIco = Join-Path $outputDirectory 'sakura-input.ico'
$outputWizardImage = Join-Path $outputDirectory 'sakura-input-installer-image.png'

if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Source icon does not exist: $source"
}

New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

if (-not ('SakuraInputIconBuilder' -as [type])) {
    $drawingReferences = @(
        Get-ChildItem -LiteralPath $PSHOME -File |
            Where-Object { $_.Name -match '^System\.(Drawing|Private\.Windows)\..*\.dll$' -or $_.Name -eq 'System.Drawing.Common.dll' } |
            Select-Object -ExpandProperty FullName
    )
    Add-Type -ReferencedAssemblies $drawingReferences -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;

public static class SakuraInputIconBuilder
{
    public static void Recolor(string sourcePath, string outputPath, double hueShiftDegrees)
    {
        using (var source = new Bitmap(sourcePath))
        using (var output = new Bitmap(source.Width, source.Height, PixelFormat.Format32bppArgb))
        {
            var area = new Rectangle(0, 0, source.Width, source.Height);
            var inputData = source.LockBits(area, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            var outputData = output.LockBits(area, ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
            var inputBytes = new byte[Math.Abs(inputData.Stride) * source.Height];
            var outputBytes = new byte[Math.Abs(outputData.Stride) * source.Height];
            Marshal.Copy(inputData.Scan0, inputBytes, 0, inputBytes.Length);

            for (var y = 0; y < source.Height; y++)
            {
                var inputRow = y * inputData.Stride;
                var outputRow = y * outputData.Stride;
                for (var x = 0; x < source.Width; x++)
                {
                    var inputIndex = inputRow + (x * 4);
                    var outputIndex = outputRow + (x * 4);
                    var alpha = inputBytes[inputIndex + 3];
                    outputBytes[outputIndex + 3] = alpha;
                    if (alpha == 0)
                    {
                        outputBytes[outputIndex] = 0;
                        outputBytes[outputIndex + 1] = 0;
                        outputBytes[outputIndex + 2] = 0;
                        continue;
                    }

                    var blue = inputBytes[inputIndex] / 255.0;
                    var green = inputBytes[inputIndex + 1] / 255.0;
                    var red = inputBytes[inputIndex + 2] / 255.0;
                    var max = Math.Max(red, Math.Max(green, blue));
                    var min = Math.Min(red, Math.Min(green, blue));
                    var delta = max - min;
                    var hue = 0.0;
                    if (delta > 0.0000001)
                    {
                        if (max == red)
                        {
                            hue = 60.0 * (((green - blue) / delta) % 6.0);
                        }
                        else if (max == green)
                        {
                            hue = 60.0 * (((blue - red) / delta) + 2.0);
                        }
                        else
                        {
                            hue = 60.0 * (((red - green) / delta) + 4.0);
                        }
                    }
                    if (hue < 0.0)
                    {
                        hue += 360.0;
                    }
                    hue = (hue + hueShiftDegrees) % 360.0;
                    if (hue < 0.0)
                    {
                        hue += 360.0;
                    }

                    var saturation = max <= 0.0000001 ? 0.0 : delta / max;
                    var value = max;
                    var chroma = value * saturation;
                    var sector = hue / 60.0;
                    var intermediate = chroma * (1.0 - Math.Abs((sector % 2.0) - 1.0));
                    var offset = value - chroma;
                    var hueRed = 0.0;
                    var hueGreen = 0.0;
                    var hueBlue = 0.0;
                    if (sector < 1.0) { hueRed = chroma; hueGreen = intermediate; }
                    else if (sector < 2.0) { hueRed = intermediate; hueGreen = chroma; }
                    else if (sector < 3.0) { hueGreen = chroma; hueBlue = intermediate; }
                    else if (sector < 4.0) { hueGreen = intermediate; hueBlue = chroma; }
                    else if (sector < 5.0) { hueRed = intermediate; hueBlue = chroma; }
                    else { hueRed = chroma; hueBlue = intermediate; }

                    outputBytes[outputIndex] = ToByte(hueBlue + offset);
                    outputBytes[outputIndex + 1] = ToByte(hueGreen + offset);
                    outputBytes[outputIndex + 2] = ToByte(hueRed + offset);
                }
            }

            Marshal.Copy(outputBytes, 0, outputData.Scan0, outputBytes.Length);
            source.UnlockBits(inputData);
            output.UnlockBits(outputData);
            output.Save(outputPath, ImageFormat.Png);
        }
    }

    public static void BuildIco(string sourcePath, string outputPath)
    {
        var sizes = new[] { 16, 20, 24, 32, 40, 48, 64, 96, 128, 256 };
        var frames = new byte[sizes.Length][];
        using (var source = new Bitmap(sourcePath))
        {
            for (var i = 0; i < sizes.Length; i++)
            {
                var size = sizes[i];
                using (var frame = new Bitmap(size, size, PixelFormat.Format32bppArgb))
                using (var graphics = Graphics.FromImage(frame))
                using (var stream = new MemoryStream())
                {
                    graphics.CompositingMode = CompositingMode.SourceCopy;
                    graphics.CompositingQuality = CompositingQuality.HighQuality;
                    graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
                    graphics.PixelOffsetMode = PixelOffsetMode.HighQuality;
                    graphics.SmoothingMode = SmoothingMode.HighQuality;
                    graphics.Clear(Color.Transparent);
                    graphics.DrawImage(source, new Rectangle(0, 0, size, size));
                    frame.Save(stream, ImageFormat.Png);
                    frames[i] = stream.ToArray();
                }
            }
        }

        using (var output = new FileStream(outputPath, FileMode.Create, FileAccess.Write, FileShare.None))
        using (var writer = new BinaryWriter(output))
        {
            writer.Write((ushort)0);
            writer.Write((ushort)1);
            writer.Write((ushort)frames.Length);
            var offset = 6 + (16 * frames.Length);
            for (var i = 0; i < sizes.Length; i++)
            {
                var size = sizes[i];
                writer.Write((byte)(size == 256 ? 0 : size));
                writer.Write((byte)(size == 256 ? 0 : size));
                writer.Write((byte)0);
                writer.Write((byte)0);
                writer.Write((ushort)1);
                writer.Write((ushort)32);
                writer.Write((uint)frames[i].Length);
                writer.Write((uint)offset);
                offset += frames[i].Length;
            }
            foreach (var frame in frames)
            {
                writer.Write(frame);
            }
        }
    }

    public static void BuildWizardImage(string sourcePath, string outputPath)
    {
        const int width = 492;
        const int height = 942;
        const int artworkSize = 942;
        using (var source = new Bitmap(sourcePath))
        using (var output = new Bitmap(width, height, PixelFormat.Format32bppArgb))
        using (var graphics = Graphics.FromImage(output))
        {
            graphics.CompositingMode = CompositingMode.SourceCopy;
            graphics.CompositingQuality = CompositingQuality.HighQuality;
            graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
            graphics.PixelOffsetMode = PixelOffsetMode.HighQuality;
            graphics.SmoothingMode = SmoothingMode.HighQuality;
            graphics.Clear(Color.Transparent);
            // Inno Setup's modern wizard uses a 492x942 (3x DPI) portrait
            // image. The square artwork is scaled to the portrait height and
            // centered, matching Sakura Editor NEXT's edge-to-edge treatment.
            graphics.DrawImage(source, new Rectangle((width - artworkSize) / 2, 0,
                artworkSize, artworkSize));
            output.Save(outputPath, ImageFormat.Png);
        }
    }

    private static byte ToByte(double value)
    {
        return (byte)Math.Max(0, Math.Min(255, Math.Round(value * 255.0)));
    }
}
'@
}

[SakuraInputIconBuilder]::Recolor($source, $outputPng, $HueShiftDegrees)
[SakuraInputIconBuilder]::BuildIco($outputPng, $outputIco)
[SakuraInputIconBuilder]::BuildWizardImage($outputPng, $outputWizardImage)

Get-Item -LiteralPath $outputPng, $outputIco, $outputWizardImage |
    Select-Object FullName, Length, LastWriteTime
