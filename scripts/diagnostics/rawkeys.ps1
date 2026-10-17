# Logs every key that reaches Windows at the Raw Input level (below all hooks and apps) for 60 s.
# If a key is missing here, the keyboard itself is not sending it (firmware lock, e.g. ASUS Win Key).
# Output: %TEMP%\monarch-rawkeys.txt. Prints key codes only while it runs; delete the file afterwards.
$out = Join-Path $env:TEMP 'monarch-rawkeys.txt'
"" | Set-Content $out
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Windows.Forms;
public class RawSink : Form {
  [StructLayout(LayoutKind.Sequential)] struct RID { public ushort Page, Usage; public uint Flags; public IntPtr Target; }
  [DllImport("user32.dll")] static extern bool RegisterRawInputDevices(RID[] r, uint n, uint sz);
  [DllImport("user32.dll")] static extern uint GetRawInputData(IntPtr h, uint cmd, byte[] data, ref uint size, uint hsz);
  public string Path;
  protected override void OnHandleCreated(EventArgs e) {
    base.OnHandleCreated(e);
    var r = new RID[] { new RID { Page = 1, Usage = 6, Flags = 0x100 /*INPUTSINK*/, Target = Handle } };
    RegisterRawInputDevices(r, 1, (uint)Marshal.SizeOf(typeof(RID)));
  }
  protected override void WndProc(ref Message m) {
    if (m.Msg == 0xFF) {
      int hsz = IntPtr.Size == 8 ? 24 : 16; uint size = 0;
      GetRawInputData(m.LParam, 0x10000003, null, ref size, (uint)hsz);
      var b = new byte[size];
      GetRawInputData(m.LParam, 0x10000003, b, ref size, (uint)hsz);
      ushort make = BitConverter.ToUInt16(b, hsz), flags = BitConverter.ToUInt16(b, hsz + 2), vkey = BitConverter.ToUInt16(b, hsz + 6);
      uint msg = BitConverter.ToUInt32(b, hsz + 8);
      if ((msg == 0x100 || msg == 0x104)) System.IO.File.AppendAllText(Path, DateTime.Now.ToString("HH:mm:ss") + " vk=0x" + vkey.ToString("x") + " make=0x" + make.ToString("x") + " e0=" + ((flags & 2) != 0) + "\r\n");
    }
    base.WndProc(ref m);
  }
}
'@
$f = New-Object RawSink; $f.Path = $out; $f.ShowInTaskbar = $false; $f.WindowState = 'Minimized'; $f.Opacity = 0
$t = New-Object System.Windows.Forms.Timer; $t.Interval = 60000; $t.Add_Tick({ $f.Close() }); $t.Start()
[System.Windows.Forms.Application]::Run($f)
