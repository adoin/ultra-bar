# Rebuild the transparent, multi-resolution application icon from native shapes.
[CmdletBinding()]param([string]$Output = "$PSScriptRoot/../assets/ultra-bar.ico")
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$images = foreach ($size in $sizes) {
    $bitmap = [Drawing.Bitmap]::new($size, $size)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = 'AntiAlias'
    $g.ScaleTransform($size / 64.0, $size / 64.0)
    $path = [Drawing.Drawing2D.GraphicsPath]::new()
    foreach ($arc in @(@(2,2,20,20,180), @(42,2,20,20,270), @(42,42,20,20,0), @(2,42,20,20,90))) {
        $path.AddArc($arc[0],$arc[1],$arc[2],$arc[3],$arc[4],90)
    }
    $path.CloseFigure()
    $base = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(255,25,34,53))
    $blue = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(255,80,172,255))
    $light = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(255,206,231,255))
    $g.FillPath($base,$path)
    $g.FillRectangle($blue,12,15,17,22); $g.FillRectangle($light,33,15,19,22)
    $g.FillRectangle($blue,12,44,40,6)
    $stream = [IO.MemoryStream]::new(); $bitmap.Save($stream,[Drawing.Imaging.ImageFormat]::Png)
    ,$stream.ToArray()
    $stream.Dispose(); $path.Dispose(); $base.Dispose(); $blue.Dispose(); $light.Dispose(); $g.Dispose(); $bitmap.Dispose()
}
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Output))) | Out-Null
$writer = [IO.BinaryWriter]::new([IO.File]::Create([IO.Path]::GetFullPath($Output)))
try {
    $writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($i=0; $i -lt $sizes.Count; $i++) {
        $dimension = if ($sizes[$i] -eq 256) { 0 } else { $sizes[$i] }
        $writer.Write([byte]$dimension); $writer.Write([byte]$dimension); $writer.Write([uint16]0)
        $writer.Write([uint16]1); $writer.Write([uint16]32)
        $writer.Write([uint32]$images[$i].Length); $writer.Write([uint32]$offset)
        $offset += $images[$i].Length
    }
    foreach ($bytes in $images) { $writer.Write([byte[]]$bytes) }
} finally { $writer.Dispose() }
