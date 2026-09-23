# Harnais de test invisible : l'application tourne sans jamais afficher de
# fenetre, et aucun dialogue systeme ne s'ouvre. Rien n'apparait a l'ecran.
#
# Usage : . .\tools\headless.ps1 ; Start-App <fichier.pdf> ; Key 0x75 ; Shot "x" ; Stop-App
# Dossier de travail : tout ce que le harnais ecrit (journal, captures) y va.
# ACRUX_TEST_DIR isole les essais : plusieurs harnais peuvent tourner en meme temps,
# chacun avec son dossier, ses captures et son instance.
if (-not $script:Dir) { $script:Dir = if ($env:ACRUX_TEST_DIR) { $env:ACRUX_TEST_DIR } else { Join-Path $env:TEMP "acrux-tests" } }
if (-not (Test-Path $script:Dir)) { New-Item -ItemType Directory -Path $script:Dir | Out-Null }
# Racine du depot : deux niveaux au-dessus de ce script.
if (-not $script:Root) { $script:Root = Split-Path -Parent (Split-Path -Parent $PSCommandPath) }

$sig = @'
[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
public struct RECT { public int Left, Top, Right, Bottom; }
'@
if (-not ("W.HL" -as [type])) { Add-Type -MemberDefinition $sig -Name HL -Namespace W | Out-Null }

# Echelle entre les pixels de la capture et ceux des messages de souris.
# En mode invisible la fenetre n'est jamais montree : elle garde la taille
# physique que le systeme lui a donnee (150 % ici), alors que le tampon dessine
# reste a la taille logique. Les coordonnees se donnent donc comme on les lit
# sur la capture, et le harnais fait la conversion.
function Update-Scale {
    $script:Scale = 1.0
    $ppm = "$script:Dir/frame.ppm"
    if (-not (Test-Path $ppm)) { return }
    $head = [System.Text.Encoding]::ASCII.GetString((Get-Content $ppm -AsByteStream -TotalCount 32))
    if ($head -notmatch 'P6\s+(\d+)\s+(\d+)') { return }
    $fw = [int]$Matches[1]
    $r = New-Object W.HL+RECT
    if (-not [W.HL]::GetClientRect($script:Hwnd, [ref]$r)) { return }
    if ($fw -gt 0 -and $r.Right -gt 0) { $script:Scale = $r.Right / $fw }
}

function Start-App {
    param([string]$Pdf, [hashtable]$Env = @{})
    # On n'arrete que l'instance que ce harnais a lancee : jamais celle de la
    # personne qui travaille, ni celle d'un autre essai en cours.
    Stop-App
    Start-Sleep -Milliseconds 200
    $env:ACRUX_HEADLESS = "1"
    $env:APPDATA = "$script:Dir/appdata"
    Remove-Item -Recurse -Force "$script:Dir/appdata" -ErrorAction SilentlyContinue
    $env:ACRUX_LOG = "$script:Dir/events.log"
    $env:ACRUX_SHOT = "$script:Dir/frame.ppm"
    $env:ACRUX_HWND = "$script:Dir/hwnd.txt"
    Remove-Item $env:ACRUX_LOG, $env:ACRUX_HWND -ErrorAction SilentlyContinue
    Remove-Item Env:ACRUX_THEME, Env:ACRUX_OPEN_FILE, Env:ACRUX_SAVE_FILE, Env:ACRUX_SAVE_DIR, Env:ACRUX_CONFIRM -ErrorAction SilentlyContinue
    foreach ($k in $Env.Keys) { Set-Item -Path "Env:$k" -Value $Env[$k] }
    # ACRUX_EXE designe un autre executable que celui de target\debug : une copie
    # figee pendant qu'une compilation le remplace, par exemple.
    $exe = if ($env:ACRUX_EXE) { $env:ACRUX_EXE } else { Join-Path $script:Root "target\debug\acrux.exe" }
    $script:Proc = Start-Process -FilePath $exe -ArgumentList $Pdf -PassThru
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 200
        if (Test-Path $env:ACRUX_HWND) { break }
    }
    if (-not (Test-Path $env:ACRUX_HWND)) { throw "la fenetre cachee n'a pas publie sa poignee" }
    $script:Hwnd = [IntPtr][int64](Get-Content $env:ACRUX_HWND)
    Start-Sleep -Milliseconds 800
    Update-Scale
}

function Stop-App {
    if ($script:Proc -and -not $script:Proc.HasExited) {
        Stop-Process -Id $script:Proc.Id -Force -ErrorAction SilentlyContinue
    }
}

function LParam($x, $y) {
    $sx = [int][Math]::Round($x * $script:Scale)
    $sy = [int][Math]::Round($y * $script:Scale)
    [IntPtr](($sy -shl 16) -bor ($sx -band 0xFFFF))
}

# Le relachement porte les bits 30 et 31 de lParam (touche deja enfoncee, puis
# relachee), comme le vrai clavier : sans eux, TranslateMessage le prend pour un
# second appui et fabrique un second WM_CHAR (deux espaces pour une barre d'espace).
$script:KeyUp = [IntPtr]0xC0000001L

function Key($vk) {
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]$vk, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 90
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]$vk, $script:KeyUp) | Out-Null
    Start-Sleep -Milliseconds 350
}

function Char($c) {
    [W.HL]::PostMessage($script:Hwnd, 0x0102, [IntPtr][int][char]$c, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 60
}

function Typing($text) { foreach ($c in $text.ToCharArray()) { Char $c } }

function Click($x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0201, [IntPtr]1, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0202, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 500
}

# Survol seul : le pointeur se pose en (x, y) sans rien cliquer. Sert a voir
# l'etat survole d'un bouton, ou a placer la souris avant une touche (la note
# se pose la ou est le pointeur).
function Hover($x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 300
}

# Trace un geste continu : bouton enfonce, une suite de points, puis relache.
# Les points sont donnes en coordonnees client : @(@(x,y), @(x,y), ...).
function Drag($points) {
    $first = $points[0]
    [W.HL]::PostMessage($script:Hwnd, 0x0201, [IntPtr]1, (LParam $first[0] $first[1])) | Out-Null
    Start-Sleep -Milliseconds 60
    foreach ($p in $points) {
        [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]1, (LParam $p[0] $p[1])) | Out-Null
        Start-Sleep -Milliseconds 12
    }
    $last = $points[$points.Length - 1]
    [W.HL]::PostMessage($script:Hwnd, 0x0202, [IntPtr]0, (LParam $last[0] $last[1])) | Out-Null
    Start-Sleep -Milliseconds 300
}

function Shot($name) {
    Start-Sleep -Milliseconds 700
    Copy-Item "$script:Dir/frame.ppm" "$script:Dir/$name.ppm" -Force
}

function Journal($pattern) {
    Select-String -Path "$script:Dir/events.log" -Pattern $pattern | ForEach-Object { $_.Line }
}

# Raccourci Ctrl+lettre (ou Ctrl+Maj+lettre) : le modificateur est poste en
# WM_KEYDOWN, ce que la fenetre invisible retient, puis la lettre arrive comme
# le vrai clavier l'enverrait, en caractere de controle.
function Chord($letter, [switch]$Shift) {
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x11, [IntPtr]1) | Out-Null
    if ($Shift) { [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x10, [IntPtr]1) | Out-Null }
    Start-Sleep -Milliseconds 60
    $code = [int][char]$letter.ToUpper() - 64
    [W.HL]::PostMessage($script:Hwnd, 0x0102, [IntPtr]$code, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 120
    if ($Shift) { [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x10, $script:KeyUp) | Out-Null }
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x11, $script:KeyUp) | Out-Null
    Start-Sleep -Milliseconds 400
}
