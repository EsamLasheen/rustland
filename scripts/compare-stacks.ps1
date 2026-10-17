# Measures a desktop stack's real cost: every process of the stack plus all descendants
# (e.g. Zebar's WebView2 renderers), CPU over a window and private memory at the end.
# Usage: .\compare-stacks.ps1 -Roots komorebi,zebar,monarch,Flow.Launcher -Seconds 60
param([string[]]$Roots, [int]$Seconds = 60)

function Get-Tree([int[]]$ids) {
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
    $set = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($i in $ids) { [void]$set.Add($i) }
    do {
        $added = 0
        foreach ($p in $all) { if ($set.Contains([int]$p.ParentProcessId) -and $set.Add([int]$p.ProcessId)) { $added++ } }
    } while ($added -gt 0)
    $set
}

$rootIds = foreach ($r in $Roots) { (Get-Process -Name $r -ErrorAction SilentlyContinue).Id }
$ids = Get-Tree $rootIds
$cpu0 = @{}; foreach ($i in $ids) { $p = Get-Process -Id $i -ErrorAction SilentlyContinue; if ($p) { $cpu0[$i] = $p.TotalProcessorTime.TotalSeconds } }
$t0 = Get-Date
Start-Sleep -Seconds $Seconds
$el = ((Get-Date) - $t0).TotalSeconds
$cores = [Environment]::ProcessorCount
$rows = foreach ($i in $ids) {
    $p = Get-Process -Id $i -ErrorAction SilentlyContinue
    if ($p -and $cpu0.ContainsKey($i)) {
        [pscustomobject]@{
            Name = $p.Name; Id = $i
            CpuPct = [math]::Round(($p.TotalProcessorTime.TotalSeconds - $cpu0[$i]) / $el / $cores * 100, 3)
            PrivateMB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
            WorkingSetMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
            Handles = $p.HandleCount; Threads = $p.Threads.Count
        }
    }
}
$rows | Sort-Object PrivateMB -Descending | Format-Table -AutoSize | Out-String -Width 160
"TOTAL processes={0} cpu%={1} private={2} MB workingset={3} MB" -f $rows.Count,
    [math]::Round(($rows | Measure-Object CpuPct -Sum).Sum, 3),
    [math]::Round(($rows | Measure-Object PrivateMB -Sum).Sum, 1),
    [math]::Round(($rows | Measure-Object WorkingSetMB -Sum).Sum, 1)
