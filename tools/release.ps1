# Assemble une version publiable dans dist/ : installateur, archive portable,
# sommes de controle et notes.
#
# Suppose que `cargo build --release --workspace` a deja tourne. C'est le meme
# script qui sert en local et dans le workflow de publication : ce qu'on teste
# sur sa machine est exactement ce qui sera publie.
#
#   .\tools\release.ps1 -Version 0.1.0

param(
    [Parameter(Mandatory = $true)][string]$Version
)

$ErrorActionPreference = 'Stop'
$racine = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location $racine

$release = Join-Path $racine 'target\release'
$dist = Join-Path $racine 'dist'
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $dist | Out-Null

foreach ($exe in 'acrux.exe', 'acr.exe', 'acrux-setup.exe') {
    $chemin = Join-Path $release $exe
    if (-not (Test-Path $chemin)) {
        throw "$exe est absent de target\release : lancez d'abord cargo build --release --workspace"
    }
}

# --- L'installateur ---------------------------------------------------------
# Le programme se recopie lui-meme en y collant les fichiers a installer.
$setup = Join-Path $dist 'acrux-setup.exe'
& (Join-Path $release 'acrux-setup.exe') --pack --out $setup `
    "acrux.exe=$release\acrux.exe" `
    "acr.exe=$release\acr.exe" `
    "LICENSE=LICENSE" `
    "LISEZ-MOI.md=README.md" `
    "acrux.ico=branding\acrux.ico"
if ($LASTEXITCODE -ne 0) { throw "la fabrication de l'installateur a echoue" }

# --- L'archive portable -----------------------------------------------------
# Pour qui ne veut rien installer : les deux executables, decompresses ou lances
# depuis une cle USB.
$portable = Join-Path $dist "acrux-$Version-portable"
New-Item -ItemType Directory -Path $portable | Out-Null
Copy-Item (Join-Path $release 'acrux.exe') $portable
Copy-Item (Join-Path $release 'acr.exe') $portable
Copy-Item 'LICENSE' $portable
Copy-Item 'README.md' (Join-Path $portable 'LISEZ-MOI.md')
Compress-Archive -Path "$portable\*" -DestinationPath "$portable.zip" -Force
Remove-Item $portable -Recurse -Force

# --- Sommes de controle -----------------------------------------------------
# Pour verifier qu'un telechargement est bien celui qui a ete publie.
$sommes = Join-Path $dist 'SHA256SUMS.txt'
Get-ChildItem $dist -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } |
    ForEach-Object {
        $h = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
        "$h  $($_.Name)"
    } | Set-Content -Path $sommes -Encoding ascii

# --- Notes de version -------------------------------------------------------
$notes = @"
## Acrux $Version

**Installer** : telechargez ``acrux-setup.exe`` et lancez-le. L'installation se
fait dans votre compte, **sans droits d'administrateur**, et se defait depuis
« Applications installees ».

**Sans installer** : ``acrux-$Version-portable.zip`` contient les deux
executables, ``acrux.exe`` (l'application) et ``acr.exe`` (la ligne de commande).

Verifiez votre telechargement avec ``SHA256SUMS.txt`` :

``````powershell
Get-FileHash .\acrux-setup.exe -Algorithm SHA256
``````

### Note

Les executables ne sont pas signes par un certificat commercial : Windows
SmartScreen affichera un avertissement au premier lancement (« Informations
complementaires » puis « Executer quand meme »). Les sommes de controle
ci-dessus permettent de verifier que le fichier est bien celui publie ici, et
tout le code est lisible dans ce depot.
"@
Set-Content -Path (Join-Path $dist 'notes.md') -Value $notes -Encoding utf8

Write-Host ''
Write-Host "Version $Version assemblee dans dist\ :"
Get-ChildItem $dist -File | ForEach-Object {
    "{0,-40} {1,8} Kio" -f $_.Name, [math]::Round($_.Length / 1KB)
}
