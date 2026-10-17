# Re-enable the Windows key on ASUS laptops (TUF / ROG) when Armoury Crate's "Win Key" lock is on.
# The lock lives in the keyboard controller: the key produces no input at all (not even Raw Input),
# so no software hook can see it. Only Armoury Crate (or Fn+Win) can switch it back; this script drives
# Armoury Crate's own "System Settings > System Configuration > Win Key" tile through UI Automation.
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\asus-winkey.ps1
$key = 'HKCU:\Software\ASUS\Armoury Crate Service\SystemFeature'
function Status { (Get-ItemProperty $key -ErrorAction SilentlyContinue).WinKeyStatus }
if ($null -eq (Status)) { 'Armoury Crate Win Key setting not found (not an ASUS machine?)'; exit 0 }
if ((Status) -eq 1) { 'Win key already enabled.'; exit 0 }

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -Namespace M -Name I -MemberDefinition '[DllImport("user32.dll")] public static extern void mouse_event(uint f,int x,int y,uint d,System.UIntPtr e); [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);'
$A = [Windows.Automation.AutomationElement]
function Cond($prop, $value) { New-Object Windows.Automation.PropertyCondition($prop, $value) }
function Wait-Find($parent, $cond, $scope = 'Descendants', $seconds = 20) {
    for ($i = 0; $i -lt $seconds * 2; $i++) { $e = $parent.FindFirst($scope, $cond); if ($e) { return $e }; Start-Sleep -Milliseconds 500 }
}
function Click-Center($el) {
    $r = $el.Current.BoundingRectangle
    $sw = [M.I]::GetSystemMetrics(0); $sh = [M.I]::GetSystemMetrics(1)
    $x = [int](($r.X + $r.Width / 2) / $sw * 65535); $y = [int](($r.Y + $r.Height / 3) / $sh * 65535)
    [M.I]::mouse_event(0x8001, $x, $y, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 250
    [M.I]::mouse_event(0x8002, $x, $y, 0, [UIntPtr]::Zero); [M.I]::mouse_event(0x8004, $x, $y, 0, [UIntPtr]::Zero)
}

Start-Process 'shell:AppsFolder\B9ECED6F.ArmouryCrate_qmba6cd70vzyy!App'
$win = Wait-Find $A::RootElement (Cond $A::NameProperty 'Armoury Crate') 'Children'
if (-not $win) { 'Armoury Crate did not open.'; exit 1 }
$settings = Wait-Find $win (Cond $A::AutomationIdProperty 'SystemSetting')
if (-not $settings) { 'System Settings button not found.'; exit 1 }
$settings.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
$tileCond = New-Object Windows.Automation.AndCondition((Cond $A::NameProperty 'Win Key'), (Cond $A::ControlTypeProperty ([Windows.Automation.ControlType]::Button)))
$tile = Wait-Find $win $tileCond
if (-not $tile) { 'Win Key tile not found.'; exit 1 }
Start-Sleep 1
Click-Center $tile
for ($i = 0; $i -lt 10 -and (Status) -ne 1; $i++) { Start-Sleep -Milliseconds 500 }
if ((Status) -eq 1) { 'Win key enabled.' } else { 'Could not confirm. Press Fn+Win once.' ; exit 1 }
