# Developer update of the installed shell: clean exit, replace the executable, start again.
$root = Split-Path $PSScriptRoot -Parent
$src = Join-Path $root 'target\release\monarch-next.exe'
$dst = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\MonarchNext\MonarchNext.exe'
$log = Join-Path $root 'dist\deploy.log'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern System.IntPtr FindWindowW(string c, string t);
[DllImport("user32.dll")] public static extern bool PostMessageW(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
'@
$stop = New-Object System.Threading.EventWaitHandle($false, 'ManualReset', 'Local\MonarchNext.Stop')
[void]$stop.Set()
$bar = [W.U]::FindWindowW('MonarchNext.Surface', "Monarch Next $([char]0xB7) bar")
if ($bar -ne [IntPtr]::Zero) { [void][W.U]::PostMessageW($bar, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
for ($i = 0; $i -lt 40 -and (Get-Process MonarchNext -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 250 }
$left = Get-Process MonarchNext -ErrorAction SilentlyContinue
if ($left) { "$(Get-Date -Format T) still running: $($left.Id -join ',')" | Out-File $log -Append; exit 1 }
[void]$stop.Reset(); $stop.Dispose()
Copy-Item $src $dst -Force
Copy-Item $src (Join-Path $root 'dist\MonarchNext-Setup.exe') -Force
Start-Process $dst -ArgumentList '--shell'
"$(Get-Date -Format T) deployed $((Get-Item $dst).LastWriteTime)" | Out-File $log -Append
