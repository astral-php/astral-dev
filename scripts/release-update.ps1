<#
  Publication manuelle assistée (ADR-019, lot 6.6) : construit l'installeur signé et prépare `latest.json`.
  Rien n'est publié : le script affiche la commande `gh` à lancer (ou à adapter) une fois le résultat relu.

  Usage (depuis la racine du dépôt) :
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = '<mot de passe>'   # facultatif si la clé n'en a pas
    ./scripts/release-update.ps1 -KeyPath C:\chemin\hors-depot\rustral-studio.key -Notes "Nouveautés…"

  La clé privée reste HORS du dépôt (jamais de commit). La clé publique correspondante est dans src-tauri/tauri.conf.json.
#>
param(
  [Parameter(Mandatory = $true)][string]$KeyPath,
  [string]$Notes = '',
  [string]$Repo = 'astral-php/rustral-studio'
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $KeyPath)) { throw "Clé introuvable : $KeyPath" }
$conf = Get-Content -Raw -Encoding UTF8 src-tauri/tauri.conf.json | ConvertFrom-Json
$version = $conf.version
$name = $conf.productName
$env:TAURI_SIGNING_PRIVATE_KEY = $KeyPath
npx tauri build
if ($LASTEXITCODE -ne 0) { throw 'Échec de la construction' }
$setup = "src-tauri/target/release/bundle/nsis/${name}_${version}_x64-setup.exe"
$sig = "$setup.sig"
if (-not (Test-Path -LiteralPath $setup) -or -not (Test-Path -LiteralPath $sig)) { throw "Artefacts introuvables : $setup(.sig)" }
$dist = "dist/$version"
New-Item -ItemType Directory -Force $dist | Out-Null
Copy-Item -LiteralPath $setup, $sig -Destination $dist -Force
$file = Split-Path $setup -Leaf
$tag = "v$version"
$url = "https://github.com/$Repo/releases/download/$tag/" + [uri]::EscapeDataString($file)
$latest = [ordered]@{
  version   = $version
  notes     = $Notes
  pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  platforms = [ordered]@{ 'windows-x86_64' = [ordered]@{ signature = (Get-Content -Raw -Encoding UTF8 $sig).Trim(); url = $url } }
}
# UTF-8 sans BOM (un BOM rend le fichier illisible pour le plugin)
[IO.File]::WriteAllText((Join-Path (Resolve-Path $dist) 'latest.json'), ($latest | ConvertTo-Json -Depth 6), (New-Object Text.UTF8Encoding($false)))
Write-Host "Prêt dans $dist :" -ForegroundColor Green
Get-ChildItem $dist | ForEach-Object { Write-Host "  $($_.Name)" }
Write-Host "`nÀ relire puis publier (non exécuté) :" -ForegroundColor Yellow
Write-Host "  gh release create $tag --repo $Repo --title `"$name $version`" --notes `"$Notes`" `"$dist/$file`" `"$dist/$file.sig`" `"$dist/latest.json`""
