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
                string appId = null;
                bool selfTest = false, stdio = false;
                for (int index = 0; index < args.Length; index++)
                {
                    if (args[index] == "--self-test") selfTest = true;
                    else if (args[index] == "--stdio") stdio = true;
                    else if (args[index] == "--aumid" && index + 1 < args.Length) appId = args[++index];
                    else return 2;
                }
                // 任务栏按 AppUserModelID 分组；与主进程一致才能合并为一个按钮，
                // 并让悬停预览同时列出主窗口和本进程的远程桌面窗口。
                if (appId != null && !Native.SetAppUserModelId(appId)) return 3;
                if (selfTest) return SelfTest.Run(appId);
                if (!stdio) return 2;
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
