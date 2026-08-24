# Crop a region of a capture at 1:1, no resampling.
# Downscaling a screenshot hides the rendering bugs worth finding.
param([string]$In, [string]$Out, [int]$X, [int]$Y, [int]$W, [int]$H)
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Bitmap]::FromFile($In)
$dst = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($dst)
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g.DrawImage($src,
  (New-Object System.Drawing.Rectangle 0, 0, $W, $H),
  (New-Object System.Drawing.Rectangle $X, $Y, $W, $H),
  [System.Drawing.GraphicsUnit]::Pixel)
$dst.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$src.Dispose()
Write-Output "cropped ${W}x${H} from ${X},${Y} -> $Out"
