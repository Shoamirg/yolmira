#Requires -Version 5.1
<#
.SYNOPSIS
  AMIRI environment probe. READ-ONLY: installs nothing, changes nothing.

.DESCRIPTION
  Answers AMIRI_ARCHITECTURE.md section 1 (environment baseline).
  Never prints secret values: API keys are reported as set/unset only,
  SSH keys by file name only, MCP servers by name only (their env/args
  often contain tokens).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\amiri-env-probe.ps1 -RepoPath D:\Projects\AMIRI
#>
[CmdletBinding()]
param(
  [string]$RepoPath = (Get-Location).Path,
  [string]$OutFile  = "amiri-env.json"
)

$ErrorActionPreference = 'Continue'
$env:WSL_UTF8 = '1'   # make wsl.exe emit UTF-8 instead of UTF-16

function Probe-Cmd {
  param([string]$Name, [string[]]$VersionArgs = @('--version'))
  # Application only: avoids npm .ps1 shims that execution policy may block
  $cmd = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $cmd) { return [ordered]@{ found = $false } }
  $ver = $null
  try {
    $out = & $cmd.Source @VersionArgs 2>&1
    $ver = ($out | ForEach-Object { "$_" } | Where-Object { $_.Trim() } | Select-Object -First 1)
  } catch { $ver = "error: $($_.Exception.Message)" }
  [ordered]@{ found = $true; path = $cmd.Source; version = "$ver".Trim() }
}

function Read-JsonSafe([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return $null }
  try { Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json } catch { $null }
}

$r = [ordered]@{ probed_at = (Get-Date).ToString('o'); probe_version = 1 }

# 1. OS
$os = Get-CimInstance Win32_OperatingSystem
$r.os = [ordered]@{
  caption = $os.Caption; version = $os.Version; build = [int]$os.BuildNumber
  arch = $env:PROCESSOR_ARCHITECTURE
  conpty_supported = ([int]$os.BuildNumber -ge 17763)   # Windows 10 1809+
  powershell = $PSVersionTable.PSVersion.ToString()
}
$r.pwsh7 = Probe-Cmd 'pwsh' @('-NoProfile','-Command','$PSVersionTable.PSVersion.ToString()')
$r.windows_terminal = [bool](Get-Command wt -ErrorAction SilentlyContinue)

# 2. Rust / Node / Tauri toolchain
$r.rustc  = Probe-Cmd 'rustc'
$r.cargo  = Probe-Cmd 'cargo'
$r.rustup = Probe-Cmd 'rustup' @('show','active-toolchain')
$r.node   = Probe-Cmd 'node'
$r.npm    = Probe-Cmd 'npm'
$r.pnpm   = Probe-Cmd 'pnpm'
$r.cargo_tauri = Probe-Cmd 'cargo-tauri' @('tauri','--version')
$r.repo_local_tauri_cli = Test-Path (Join-Path $RepoPath 'node_modules\.bin\tauri.cmd')

$wv = $null
foreach ($k in @(
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
  'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')) {
  $p = Get-ItemProperty -Path $k -ErrorAction SilentlyContinue
  if ($p -and $p.pv) { $wv = $p.pv; break }
}
$r.webview2 = [ordered]@{ found = [bool]$wv; version = $wv }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$msvc = $null
if (Test-Path $vswhere) {
  $msvc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
}
$r.msvc_build_tools = [ordered]@{ found = [bool]$msvc; path = $msvc }

# 3. Claude Code
$r.claude = Probe-Cmd 'claude'
$claudeJson = Read-JsonSafe (Join-Path $HOME '.claude.json')
$r.claude_config = [ordered]@{
  dot_claude_dir = Test-Path (Join-Path $HOME '.claude')
  user_mcp_servers = @()
  project_mcp_servers = [ordered]@{}
}
if ($claudeJson) {
  if ($claudeJson.mcpServers) { $r.claude_config.user_mcp_servers = @($claudeJson.mcpServers.PSObject.Properties.Name) }
  if ($claudeJson.projects) {
    foreach ($p in $claudeJson.projects.PSObject.Properties) {
      if ($p.Value.mcpServers) {
        $names = @($p.Value.mcpServers.PSObject.Properties.Name)
        if ($names.Count) { $r.claude_config.project_mcp_servers[$p.Name] = $names }
      }
    }
  }
}
$r.claude_skills_dir = Test-Path (Join-Path $HOME '.claude\skills')

# 4. Git
$r.git = Probe-Cmd 'git'
$r.gh  = Probe-Cmd 'gh'

# 5. SSH
$r.ssh = Probe-Cmd 'ssh' @('-V')
$agent = Get-Service ssh-agent -ErrorAction SilentlyContinue
$sshDir = Join-Path $HOME '.ssh'
$r.ssh_env = [ordered]@{
  agent_service = if ($agent) { "$($agent.Status)/$($agent.StartType)" } else { 'not installed' }
  key_files     = @(Get-ChildItem $sshDir -Filter 'id_*' -File -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name -notlike '*.pub' } | ForEach-Object Name)
  known_hosts_entries = if (Test-Path "$sshDir\known_hosts") { (Get-Content "$sshDir\known_hosts" | Measure-Object).Count } else { 0 }
  config_hosts  = @(if (Test-Path "$sshDir\config") {
                    Select-String -Path "$sshDir\config" -Pattern '^\s*Host\s+(.+)$' |
                    ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() } })
  config_uses_proxyjump = if (Test-Path "$sshDir\config") { [bool](Select-String -Path "$sshDir\config" -Pattern '^\s*(ProxyJump|ProxyCommand)\b' -Quiet) } else { $false }
}

# 6. WSL
$wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
$r.wsl = [ordered]@{ found = [bool]$wsl }
if ($wsl) {
  $r.wsl.version_info = @(& wsl.exe --version 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ })
  $r.wsl.distros      = @(& wsl.exe -l -v 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ })
}

# 7. Docker
$r.docker = Probe-Cmd 'docker'
$r.docker_desktop_running = [bool](Get-Process 'Docker Desktop','com.docker.backend' -ErrorAction SilentlyContinue)

# 8. OpenAI / Codex
$r.codex = Probe-Cmd 'codex'
$codexToml = Join-Path $HOME '.codex\config.toml'
$r.codex_config = [ordered]@{
  config_exists = Test-Path $codexToml
  auth_file_exists = Test-Path (Join-Path $HOME '.codex\auth.json')   # existence only, never read
  mcp_servers = @(if (Test-Path $codexToml) {
                  Select-String -Path $codexToml -Pattern '^\s*\[mcp_servers\.([^\]\.]+)\]' |
                  ForEach-Object { $_.Matches[0].Groups[1].Value } })
}
$r.api_keys_present = [ordered]@{   # booleans only
  OPENAI_API_KEY    = [bool]$env:OPENAI_API_KEY
  ANTHROPIC_API_KEY = [bool]$env:ANTHROPIC_API_KEY
}

# 9. Obsidian
$obsExe = Join-Path $env:LOCALAPPDATA 'Programs\Obsidian\Obsidian.exe'
$obsCfg = Read-JsonSafe (Join-Path $env:APPDATA 'obsidian\obsidian.json')
$r.obsidian = [ordered]@{ installed = (Test-Path $obsExe); vaults = @() }
if ($obsCfg -and $obsCfg.vaults) {
  $r.obsidian.vaults = @($obsCfg.vaults.PSObject.Properties | ForEach-Object {
    [ordered]@{ path = $_.Value.path; exists = (Test-Path -LiteralPath $_.Value.path)
                has_local_rest_api = (Test-Path -LiteralPath (Join-Path $_.Value.path '.obsidian\plugins\obsidian-local-rest-api')) }
  })
}

# 10. Misc runtimes relevant to project work
$r.python = Probe-Cmd 'python'
$r.uv     = Probe-Cmd 'uv'
$r.nvidia_smi = Probe-Cmd 'nvidia-smi' @('--query-gpu=name,driver_version,memory.total','--format=csv,noheader')

# 11. Repository
$r.repo = [ordered]@{ path = $RepoPath; is_git = $false }
if ($r.git.found -and (Test-Path $RepoPath)) {
  $top = & git -C $RepoPath rev-parse --show-toplevel 2>$null
  if ($LASTEXITCODE -eq 0) {
    $r.repo.is_git = $true
    $r.repo.toplevel = $top
    $r.repo.branch = (& git -C $RepoPath branch --show-current 2>$null)
    $r.repo.head = (& git -C $RepoPath rev-parse --short HEAD 2>$null)
    $r.repo.dirty_entries = (@(& git -C $RepoPath status --porcelain 2>$null)).Count
  }
  $r.repo.top_level_entries = @(Get-ChildItem -LiteralPath $RepoPath -Force -ErrorAction SilentlyContinue | ForEach-Object Name)
}

$r | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutFile -Encoding UTF8

# Human summary
$rows = foreach ($n in 'rustc','cargo','node','npm','cargo_tauri','claude','codex','git','ssh','docker','python','nvidia_smi') {
  $v = $r[$n]; [pscustomobject]@{ Tool = $n; Found = $v.found; Version = $v.version }
}
$rows | Format-Table -AutoSize
"WebView2: $($r.webview2.found)  MSVC: $($r.msvc_build_tools.found)  WSL: $($r.wsl.found)  ConPTY: $($r.os.conpty_supported)"
"Obsidian vaults: $(@($r.obsidian.vaults).Count)  Claude MCP servers: $(@($r.claude_config.user_mcp_servers).Count)  Codex MCP servers: $(@($r.codex_config.mcp_servers).Count)"
"Full report written to $OutFile (contains no secret values; review before sharing)."
