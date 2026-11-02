using System;
using System.Windows.Forms;

namespace MXterm.RdpHost
{
    internal static class Program
    {
        [STAThread]
        private static int Main(string[] args)
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.SetUnhandledExceptionMode(UnhandledExceptionMode.ThrowException);
            try
            {
                if (args.Length == 1 && args[0] == "--self-test") return SelfTest.Run();
                if (args.Length != 1 || args[0] != "--stdio") return 2;
                using (var host = new HostWindow())
                {
                    IntPtr hwnd = host.Handle;
                    Protocol.Write(new { @event = "ready", version = Protocol.Version, hwnd = hwnd.ToInt64() });
                    Protocol.Start(host);
                    Application.Run(host);
                }
                return 0;
            }
            catch (Exception error)
            {
                Protocol.Write(new { @event = "failed", code = "rdp_host_failed", hresult = error.HResult });
                return 1;
            }
        }
    }
}
