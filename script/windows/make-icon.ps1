<#
.SYNOPSIS
Make Glasscode's application icon from assets\glasscode-icon.svg: assets\glasscode-icon-1024.png and
crates\glasscode\resources\Glasscode.ico, both with a transparent background.

.DESCRIPTION
  script\windows\make-icon.ps1           # make the PNG and the ICO from the SVG, then check them
  script\windows\make-icon.ps1 -Check    # only check the PNG and the ICO (no browser needed)

Windows PowerShell runs a script only when its execution policy lets it:
  powershell -NoProfile -ExecutionPolicy Bypass -File script\windows\make-icon.ps1

The check passes when, in the PNG and in each image of the ICO, each corner pixel is transparent
(alpha 0) and the centre pixel is opaque (alpha 255). A dark tile on a white square, as the taskbar
showed it, fails.

Windows has no SVG renderer that a script can call, so Microsoft Edge or Google Chrome draws the SVG
without a window (headless); ICON_BROWSER names another Chromium browser. The ICO has 16, 24, 32, 48
and 64 as bitmaps and 256 as PNG. Each size is drawn from the SVG, not scaled down from the 1024
image. The bitmap sizes are drawn at eight times their size and averaged down to it: the browser's
edge smoothing alone leaves alpha up to 12 in corner pixels of the 16 px image, outside the rounded
tile. Glasscode.icns for macOS is not made here.
#>
[CmdletBinding()]
param(
    # Check the PNG and the ICO only; make nothing.
    [switch]$Check
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$svgName = 'assets\glasscode-icon.svg'
$pngName = 'assets\glasscode-icon-1024.png'
$icoName = 'crates\glasscode\resources\Glasscode.ico'
# Windows takes the small icons from bitmaps; 256 is PNG, which keeps the file small.
$bitmapSizes = @(16, 24, 32, 48, 64)
$pngSize = 256
$supersample = 8

# Microsoft Edge comes with Windows; Chrome is the second choice.
function Find-Browser {
    if ($env:ICON_BROWSER) {
        if (-not (Test-Path -LiteralPath $env:ICON_BROWSER -PathType Leaf)) {
            throw "$env:ICON_BROWSER does not exist"
        }
        return $env:ICON_BROWSER
    }
    $folders = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, $env:LOCALAPPDATA) | Where-Object { $_ }
    $candidates = @('Microsoft\Edge\Application\msedge.exe', 'Google\Chrome\Application\chrome.exe') |
        ForEach-Object { $program = $_; $folders | ForEach-Object { Join-Path $_ $program } }
    return $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}

# Draw the SVG at Size x Size pixels on a transparent background, into a PNG file in Work.
function Invoke-Render {
    param([string]$Browser, [string]$Work, [int]$Size)
    $page = Join-Path $Work "icon-$Size.html"
    $shot = Join-Path $Work "icon-$Size.png"
    $log = Join-Path $Work "browser-$Size.log"
    # An <img> of the given size makes the browser draw the vectors at that size.
    $source = [System.Net.WebUtility]::HtmlEncode(([Uri](Join-Path $root $svgName)).AbsoluteUri)
    $style = 'html,body{margin:0;background:transparent;overflow:hidden}img{display:block}'
    $html = "<!doctype html><html><head><meta charset=`"utf-8`"><style>$style</style></head>" +
        "<body><img src=`"$source`" width=`"$Size`" height=`"$Size`" alt=`"`"></body></html>"
    [System.IO.File]::WriteAllText($page, $html)
    # Start-Process joins the arguments without quotation marks, so a path with a space needs them.
    # The profile of its own makes a new browser process even when the person's browser is open.
    $arguments = @(
        '--headless', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-extensions',
        "`"--user-data-dir=$(Join-Path $Work 'profile')`"", '--hide-scrollbars',
        '--force-device-scale-factor=1', '--force-color-profile=srgb', '--default-background-color=00000000',
        "--window-size=$Size,$Size", "`"--screenshot=$shot`"", ([Uri]$page).AbsoluteUri
    )
    $process = Start-Process -FilePath $Browser -ArgumentList $arguments -Wait -PassThru -NoNewWindow `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $shot -PathType Leaf)) {
        $output = (Get-Content -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue) -join "`n"
        throw "$(Split-Path -Leaf $Browser) made no $Size px image (exit code $($process.ExitCode)):`n$output"
    }
    $pixels = Read-Pixels $shot
    if ($pixels.Width -ne $Size -or $pixels.Height -ne $Size) {
        throw "$(Split-Path -Leaf $Browser) drew $($pixels.Width)x$($pixels.Height) pixels, not $Size x $Size"
    }
    return $shot
}

# The pixels of an image file as top-down rows of blue, green, red, alpha (alpha not premultiplied).
function Read-Pixels {
    param([string]$Path)
    return Read-PixelsFromBytes ([System.IO.File]::ReadAllBytes($Path))
}

function Read-PixelsFromBytes {
    param([byte[]]$Data)
    $stream = New-Object System.IO.MemoryStream (, $Data)
    $bitmap = New-Object System.Drawing.Bitmap $stream
    try {
        $area = New-Object System.Drawing.Rectangle 0, 0, $bitmap.Width, $bitmap.Height
        $locked = $bitmap.LockBits($area, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
            [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $bytes = New-Object byte[] ($bitmap.Width * $bitmap.Height * 4)
            for ($y = 0; $y -lt $bitmap.Height; $y++) {
                $row = [IntPtr]::Add($locked.Scan0, $y * $locked.Stride)
                [System.Runtime.InteropServices.Marshal]::Copy($row, $bytes, $y * $bitmap.Width * 4, $bitmap.Width * 4)
            }
        } finally {
            $bitmap.UnlockBits($locked)
        }
        return [pscustomobject]@{ Width = $bitmap.Width; Height = $bitmap.Height; Bytes = $bytes }
    } finally {
        $bitmap.Dispose()
        $stream.Dispose()
    }
}

# Average each Factor x Factor block into one pixel, weighted by alpha, so that a pixel's alpha is
# the share of it that the shape covers.
function Get-Downsampled {
    param($Pixels, [int]$Factor)
    $size = $Pixels.Width / $Factor
    $source = $Pixels.Bytes
    $out = New-Object byte[] ($size * $size * 4)
    for ($y = 0; $y -lt $size; $y++) {
        for ($x = 0; $x -lt $size; $x++) {
            $alpha = 0; $blue = 0; $green = 0; $red = 0
            for ($dy = 0; $dy -lt $Factor; $dy++) {
                $i = (($y * $Factor + $dy) * $Pixels.Width + $x * $Factor) * 4
                for ($dx = 0; $dx -lt $Factor; $dx++) {
                    $a = $source[$i + 3]
                    $alpha += $a
                    $blue += $source[$i] * $a
                    $green += $source[$i + 1] * $a
                    $red += $source[$i + 2] * $a
                    $i += 4
                }
            }
            $o = ($y * $size + $x) * 4
            $average = [Math]::Round($alpha / ($Factor * $Factor))
            if ($average -gt 0) {
                $out[$o] = [Math]::Round($blue / $alpha)
                $out[$o + 1] = [Math]::Round($green / $alpha)
                $out[$o + 2] = [Math]::Round($red / $alpha)
                $out[$o + 3] = $average
            }
        }
    }
    return [pscustomobject]@{ Width = $size; Height = $size; Bytes = $out }
}

# An ICO image as a bitmap: the header, the colour rows bottom-up, then the AND mask (a bit of 1 is
# transparent). Windows draws a 32-bit image with its alpha; the mask is for programs that do not.
function Get-IconBitmap {
    param($Pixels)
    $size = $Pixels.Width
    $stride = $size * 4
    $maskStride = [int][Math]::Ceiling($size / 32) * 4
    $stream = New-Object System.IO.MemoryStream
    $writer = New-Object System.IO.BinaryWriter $stream
    # BITMAPINFOHEADER. The height counts the colour rows and the mask rows.
    $writer.Write([uint32]40)
    $writer.Write([int32]$size)
    $writer.Write([int32]($size * 2))
    $writer.Write([uint16]1)
    $writer.Write([uint16]32)
    $writer.Write([uint32]0)
    $writer.Write([uint32](($stride + $maskStride) * $size))
    $writer.Write([int32]0)
    $writer.Write([int32]0)
    $writer.Write([uint32]0)
    $writer.Write([uint32]0)
    for ($y = $size - 1; $y -ge 0; $y--) {
        $writer.Write($Pixels.Bytes, $y * $stride, $stride)
    }
    for ($y = $size - 1; $y -ge 0; $y--) {
        $row = New-Object byte[] $maskStride
        for ($x = 0; $x -lt $size; $x++) {
            if ($Pixels.Bytes[($y * $size + $x) * 4 + 3] -eq 0) {
                $row[$x -shr 3] = $row[$x -shr 3] -bor (0x80 -shr ($x -band 7))
            }
        }
        $writer.Write($row)
    }
    $writer.Flush()
    return , $stream.ToArray()
}

# Images is a list of @{ Size; Data }, Data being a bitmap from Get-IconBitmap or a PNG file.
function Write-Icon {
    param([string]$Path, $Images)
    $stream = New-Object System.IO.MemoryStream
    $writer = New-Object System.IO.BinaryWriter $stream
    # ICONDIR: reserved, type 1 (icon), the number of images.
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$Images.Count)
    $offset = 6 + 16 * $Images.Count
    foreach ($image in $Images) {
        # ICONDIRENTRY. A width and height of 0 mean 256.
        $writer.Write([byte]($image.Size % 256))
        $writer.Write([byte]($image.Size % 256))
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$image.Data.Length)
        $writer.Write([uint32]$offset)
        $offset += $image.Data.Length
    }
    foreach ($image in $Images) {
        $writer.Write($image.Data)
    }
    $writer.Flush()
    [System.IO.File]::WriteAllBytes($Path, $stream.ToArray())
}

# The images of an ICO file: their size, their kind (BMP or PNG) and a way to read an alpha value.
function Read-Icon {
    param([string]$Path)
    $data = [System.IO.File]::ReadAllBytes($Path)
    if ($data.Length -lt 6 -or [BitConverter]::ToUInt16($data, 0) -ne 0 -or [BitConverter]::ToUInt16($data, 2) -ne 1) {
        throw "$Path is not an icon file"
    }
    $count = [BitConverter]::ToUInt16($data, 4)
    for ($n = 0; $n -lt $count; $n++) {
        $entry = 6 + 16 * $n
        $length = [BitConverter]::ToUInt32($data, $entry + 8)
        $offset = [BitConverter]::ToUInt32($data, $entry + 12)
        $image = New-Object byte[] $length
        [Array]::Copy($data, $offset, $image, 0, $length)
        $isPng = $length -ge 8 -and $image[0] -eq 0x89 -and $image[1] -eq 0x50 -and $image[2] -eq 0x4E -and $image[3] -eq 0x47
        if ($isPng) {
            $pixels = Read-PixelsFromBytes $image
            [pscustomobject]@{ Kind = 'PNG'; Width = $pixels.Width; Height = $pixels.Height; Bits = 32; Pixels = $pixels; Bitmap = $null }
        } else {
            $header = [BitConverter]::ToUInt32($image, 0)
            $width = [BitConverter]::ToInt32($image, 4)
            $height = [int]([BitConverter]::ToInt32($image, 8) / 2)
            $bits = [BitConverter]::ToUInt16($image, 14)
            [pscustomobject]@{ Kind = 'BMP'; Width = $width; Height = $height; Bits = $bits; Pixels = $null; Bitmap = @{ Data = $image; Header = $header } }
        }
    }
}

# The alpha of the pixel at x, y (from the top left) of a PNG, or of a 32-bit bitmap of an ICO.
function Get-Alpha {
    param($Image, [int]$X, [int]$Y)
    if ($Image.Pixels) {
        return $Image.Pixels.Bytes[($Y * $Image.Width + $X) * 4 + 3]
    }
    # The rows of a bitmap go bottom-up.
    $row = $Image.Height - 1 - $Y
    return $Image.Bitmap.Data[$Image.Bitmap.Header + ($row * $Image.Width + $X) * 4 + 3]
}

# One line for an image; true when its corners are transparent and its centre is opaque.
function Test-Image {
    param([string]$Name, $Image)
    $label = '{0,-40} {1,4}x{2,-4} {3}' -f $Name, $Image.Width, $Image.Height, $Image.Kind
    if ($Image.Bits -ne 32) {
        Write-Host "$label  FAIL: $($Image.Bits)-bit, not 32-bit with alpha"
        return $false
    }
    $right = $Image.Width - 1
    $bottom = $Image.Height - 1
    $corners = @((Get-Alpha $Image 0 0), (Get-Alpha $Image $right 0), (Get-Alpha $Image 0 $bottom), (Get-Alpha $Image $right $bottom))
    $centre = Get-Alpha $Image ([int][Math]::Floor($Image.Width / 2)) ([int][Math]::Floor($Image.Height / 2))
    $passed = (@($corners | Where-Object { $_ -ne 0 }).Count -eq 0) -and $centre -eq 255
    $verdict = if ($passed) { 'ok' } else { 'FAIL' }
    Write-Host "$label  corner alpha $($corners -join ' ')  centre alpha $centre  $verdict"
    return $passed
}

function Test-Icon {
    $failures = @()
    $png = Read-Pixels (Join-Path $root $pngName)
    $large = [pscustomobject]@{ Kind = 'PNG'; Width = $png.Width; Height = $png.Height; Bits = 32; Pixels = $png; Bitmap = $null }
    if ($png.Width -ne 1024 -or $png.Height -ne 1024) {
        $failures += "$pngName is $($png.Width)x$($png.Height), not 1024x1024"
    }
    if (-not (Test-Image $pngName $large)) {
        $failures += "$pngName has an opaque corner or a transparent centre"
    }
    $images = @(Read-Icon (Join-Path $root $icoName))
    $expected = @($bitmapSizes | ForEach-Object { "$_ BMP" }) + "$pngSize PNG"
    $found = @($images | ForEach-Object { "$($_.Width) $($_.Kind)" })
    if (($found -join ', ') -ne ($expected -join ', ')) {
        $failures += "$icoName has $($found -join ', '); expected $($expected -join ', ')"
    }
    foreach ($image in $images) {
        if (-not (Test-Image "$icoName" $image)) {
            $failures += "$icoName $($image.Width)x$($image.Height) has an opaque corner or a transparent centre"
        }
    }
    if ($failures.Count -gt 0) {
        Write-Host "The icon check failed:`n  $($failures -join "`n  ")"
        exit 1
    }
    Write-Host 'The icon check passed: each corner pixel is transparent and each centre pixel is opaque.'
}

if (-not $Check) {
    $browser = Find-Browser
    if (-not $browser) {
        throw 'Neither Microsoft Edge nor Google Chrome was found: the script draws the SVG with one of them. Install one, or set ICON_BROWSER to a Chromium browser'
    }
    Write-Host "Drawing $svgName with $browser $((Get-Item -LiteralPath $browser).VersionInfo.ProductVersion)"
    $work = Join-Path ([System.IO.Path]::GetTempPath()) "glasscode-icon-$([Guid]::NewGuid())"
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $large = Invoke-Render $browser $work 1024
        $images = @()
        foreach ($size in $bitmapSizes) {
            $drawn = Read-Pixels (Invoke-Render $browser $work ($size * $supersample))
            $images += @{ Size = $size; Data = (Get-IconBitmap (Get-Downsampled $drawn $supersample)) }
        }
        $images += @{ Size = $pngSize; Data = [System.IO.File]::ReadAllBytes((Invoke-Render $browser $work $pngSize)) }
        Copy-Item -LiteralPath $large -Destination (Join-Path $root $pngName) -Force
        Write-Icon (Join-Path $root $icoName) $images
        Write-Host "Made $pngName and $icoName"
    } finally {
        # The browser's helper processes can hold the profile a moment after it exits.
        try {
            Remove-Item -LiteralPath $work -Recurse -Force
        } catch {
            Write-Warning "could not remove the temporary folder ${work}: $_"
        }
    }
}
Test-Icon
