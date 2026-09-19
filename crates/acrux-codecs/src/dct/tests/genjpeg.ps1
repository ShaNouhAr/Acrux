# Génère les vecteurs de référence de vectors.rs avec l'encodeur et le décodeur
# JPEG de GDI+ (System.Drawing). À lancer dans PowerShell sous Windows ; les
# fichiers produits ont été convertis en Rust (hexadécimal + pixels).
Add-Type -AssemblyName System.Drawing
$dir = $PSScriptRoot  # les .jpg et .ref.txt servent à produire vectors.rs

function Make-Jpeg($name, $w, $h, $quality, $pixelFn) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    for ($y = 0; $y -lt $h; $y++) {
        for ($x = 0; $x -lt $w; $x++) {
            $c = & $pixelFn $x $y
            $bmp.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(255, $c[0], $c[1], $c[2]))
        }
    }
    $codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
    $params = New-Object System.Drawing.Imaging.EncoderParameters 1
    $params.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), ([long]$quality)
    $path = Join-Path $dir "$name.jpg"
    $bmp.Save($path, $codec, $params)
    $bmp.Dispose()
    # Relecture par le décodeur GDI+ pour obtenir les pixels de référence.
    $img = New-Object System.Drawing.Bitmap $path
    $ref = New-Object System.Collections.Generic.List[string]
    for ($y = 0; $y -lt $h; $y++) {
        $row = @()
        for ($x = 0; $x -lt $w; $x++) {
            $p = $img.GetPixel($x, $y)
            $row += ("{0},{1},{2}" -f $p.R, $p.G, $p.B)
        }
        $ref.Add(($row -join ' '))
    }
    $img.Dispose()
    [System.IO.File]::WriteAllLines((Join-Path $dir "$name.ref.txt"), $ref)
    $bytes = [System.IO.File]::ReadAllBytes($path)
    "$name : $($bytes.Length) octets"
}

Make-Jpeg "grad16" 16 16 90 { param($x, $y) @(($x * 16), ($y * 16), 128) }
Make-Jpeg "grad37x29" 37 29 85 { param($x, $y) @([int](255 * $x / 36), [int](255 * $y / 28), [int](255 - 255 * $x / 36)) }
Make-Jpeg "gray16" 16 16 95 { param($x, $y) $v = ($x + $y) * 8; @($v, $v, $v) }
