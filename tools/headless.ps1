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
# Un type a part pour la molette : un type deja charge dans une session ne se
# redefinit pas, et W.HL a pu l'etre par une version plus ancienne du harnais.
$sig2 = @'
[DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
public struct POINT { public int X, Y; }
'@
if (-not ("W.HL2" -as [type])) { Add-Type -MemberDefinition $sig2 -Name HL2 -Namespace W | Out-Null }
# Encore un type a part pour le redimensionnement, pour la meme raison.
$sig3 = @'
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
'@
if (-not ("W.HL3" -as [type])) { Add-Type -MemberDefinition $sig3 -Name HL3 -Namespace W | Out-Null }

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

# -Shift tient Maj enfoncee le temps de la touche (Maj+F3, Maj+Entree, Maj+F4).
function Key($vk, [switch]$Shift) {
    if ($Shift) {
        [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x10, [IntPtr]1) | Out-Null
        Start-Sleep -Milliseconds 60
    }
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]$vk, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 90
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]$vk, $script:KeyUp) | Out-Null
    if ($Shift) {
        Start-Sleep -Milliseconds 60
        [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x10, $script:KeyUp) | Out-Null
    }
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

# Clic droit en (x, y) : survol, appui, relachement, comme la vraie souris.
# Le menu contextuel s'ouvre a l'appui ; le relachement sur place ne choisit
# rien (il faut bouger, ou cliquer un element).
function RightClick($x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0204, [IntPtr]2, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0205, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 500
}

# Clic du milieu en (x, y) : survol, appui (MK_MBUTTON), relachement. Sur un
# onglet, il le ferme ; sur la page, il la saisit pour la faire glisser.
function MiddleClick($x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0207, [IntPtr]0x10, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 100
    [W.HL]::PostMessage($script:Hwnd, 0x0208, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 500
}

# Maj+F10 : l'autre facon d'ouvrir le menu contextuel au clavier (la touche
# << menu >> du clavier est Key 0x5D). F10 est une touche systeme : elle
# arrive en WM_SYSKEYDOWN, pas en WM_KEYDOWN.
function ShiftF10 {
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x10, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 60
    [W.HL]::PostMessage($script:Hwnd, 0x0104, [IntPtr]0x79, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 90
    [W.HL]::PostMessage($script:Hwnd, 0x0105, [IntPtr]0x79, $script:KeyUp) | Out-Null
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x10, $script:KeyUp) | Out-Null
    Start-Sleep -Milliseconds 350
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
# -Shift tient Maj enfoncee tout le geste : Maj simulee pour la fenetre
# invisible, et MK_SHIFT (4) dans wParam des mouvements, comme la vraie souris
# (un carre, un cercle, une ligne a 45 degres). -Hold ne relache pas le bouton :
# on capture l'apercu du geste, puis Release le termine.
function Drag($points, [switch]$Shift, [switch]$Hold) {
    $first = $points[0]
    $buttons = 1
    if ($Shift) {
        [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x10, [IntPtr]1) | Out-Null
        Start-Sleep -Milliseconds 60
        $buttons = 5
    }
    [W.HL]::PostMessage($script:Hwnd, 0x0201, [IntPtr]$buttons, (LParam $first[0] $first[1])) | Out-Null
    Start-Sleep -Milliseconds 60
    foreach ($p in $points) {
        [W.HL]::PostMessage($script:Hwnd, 0x0200, [IntPtr]$buttons, (LParam $p[0] $p[1])) | Out-Null
        Start-Sleep -Milliseconds 12
    }
    $last = $points[$points.Length - 1]
    if (-not $Hold) {
        [W.HL]::PostMessage($script:Hwnd, 0x0202, [IntPtr]0, (LParam $last[0] $last[1])) | Out-Null
    }
    if ($Shift) {
        Start-Sleep -Milliseconds 60
        [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x10, $script:KeyUp) | Out-Null
    }
    Start-Sleep -Milliseconds 300
}

# Relache le bouton gauche en (x, y) : la fin d'un Drag -Hold.
function Release($x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x0202, [IntPtr]0, (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 300
}

# Molette : $notches crans (negatif vers le bas) avec le pointeur en (x, y),
# coordonnees lues sur la capture. WM_MOUSEWHEEL porte des coordonnees
# d'ECRAN, a la difference des autres messages de souris : le harnais applique
# l'echelle, puis convertit le point client en point d'ecran, comme le ferait
# Windows. Les fractions de cran sont permises (0.25 : le quart de cran d'un
# pave tactile). -Ctrl tient Ctrl enfonce le temps du cran : Ctrl+molette
# zoome vers le pointeur.
function Wheel($x, $y, [double]$notches, [switch]$Ctrl) {
    $p = New-Object W.HL2+POINT
    $p.X = [int][Math]::Round($x * $script:Scale)
    $p.Y = [int][Math]::Round($y * $script:Scale)
    [W.HL2]::ClientToScreen($script:Hwnd, [ref]$p) | Out-Null
    $units = [int][Math]::Round($notches * 120)
    $w = [IntPtr][int64](($units -band 0xFFFF) * 65536)
    $l = [IntPtr][int64]((($p.Y -band 0xFFFF) * 65536) -bor ($p.X -band 0xFFFF))
    if ($Ctrl) {
        [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x11, [IntPtr]1) | Out-Null
        Start-Sleep -Milliseconds 60
    }
    [W.HL]::PostMessage($script:Hwnd, 0x020A, $w, $l) | Out-Null
    if ($Ctrl) {
        Start-Sleep -Milliseconds 60
        [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x11, $script:KeyUp) | Out-Null
    }
    Start-Sleep -Milliseconds 350
}

# Capture immediate, sans attendre que les rendus arrivent : ce qu'on voit
# juste apres un geste (l'apercu d'une page pendant son rendu, par exemple).
function ShotNow($name) {
    Start-Sleep -Milliseconds 40
    Copy-Item "$script:Dir/frame.ppm" "$script:Dir/$name.ppm" -Force
}

# Redimensionne la fenetre invisible (largeur et hauteur exterieures, en
# pixels physiques) pour essayer une fenetre etroite. SWP_NOMOVE, SWP_NOZORDER
# et SWP_NOACTIVATE, sans SWP_SHOWWINDOW : la fenetre reste cachee et ne
# prend pas le premier plan. L'echelle des coordonnees est recalculee.
function Resize($w, $h) {
    [W.HL3]::SetWindowPos($script:Hwnd, [IntPtr]::Zero, 0, 0, $w, $h, 0x16) | Out-Null
    Start-Sleep -Milliseconds 800
    Update-Scale
}

# Touche avec Ctrl enfonce (Ctrl+Fin : 0x23, Ctrl+Origine : 0x24).
function KeyCtrl($vk) {
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x11, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 60
    Key $vk
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x11, $script:KeyUp) | Out-Null
    Start-Sleep -Milliseconds 200
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

# Alt+touche (Alt+fleche gauche : 0x25, Alt+fleche droite : 0x27). Alt fait
# arriver la touche en WM_SYSKEYDOWN ; le bit 29 de lParam dit qu'Alt est
# enfoncee, et c'est lui que lit l'application (GetKeyState ne voit rien en
# mode invisible).
function AltKey($vk) {
    [W.HL]::PostMessage($script:Hwnd, 0x0104, [IntPtr]$vk, [IntPtr]0x20000001) | Out-Null
    Start-Sleep -Milliseconds 90
    [W.HL]::PostMessage($script:Hwnd, 0x0105, [IntPtr]$vk, [IntPtr]0xE0000001L) | Out-Null
    Start-Sleep -Milliseconds 350
}

# Bouton lateral de la souris en (x, y) : 1 = precedent (bouton 4),
# 2 = suivant (bouton 5). Le numero du bouton est dans le mot fort de wParam.
function XButton($n, $x, $y) {
    [W.HL]::PostMessage($script:Hwnd, 0x020B, [IntPtr]($n -shl 16), (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 90
    [W.HL]::PostMessage($script:Hwnd, 0x020C, [IntPtr]($n -shl 16), (LParam $x $y)) | Out-Null
    Start-Sleep -Milliseconds 350
}

# Touche avec Ctrl et Maj enfoncees : Ctrl+Maj+Plus (0xBB) et Ctrl+Maj+Moins
# (0xBD) tournent la vue, Ctrl+Maj+Tab (0x09) passe a l'onglet precedent.
function CtrlShiftKey($vk) {
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x11, [IntPtr]1) | Out-Null
    [W.HL]::PostMessage($script:Hwnd, 0x0100, [IntPtr]0x10, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 60
    Key $vk
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x10, $script:KeyUp) | Out-Null
    [W.HL]::PostMessage($script:Hwnd, 0x0101, [IntPtr]0x11, $script:KeyUp) | Out-Null
    Start-Sleep -Milliseconds 200
}
