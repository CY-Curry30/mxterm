using System;
using System.IO;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;

namespace MXterm.RdpHost
{
    // Private inherited stdin/stdout only. No listening socket, command-line
    // secrets, credential files, endpoint logging, or shared named-pipe address.
    internal static class Protocol
    {
        internal const int Version = 1;
        private static readonly object OutputLock = new object();
        private static readonly StreamWriter Output = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true };

        internal static void Write(object value)
        {
            lock (OutputLock) Output.WriteLine(new JavaScriptSerializer().Serialize(value));
        }

        internal static void Start(HostWindow host)
        {
            var reader = new Thread(delegate()
            {
                try
                {
                    using (var input = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false, true)))
                    {
                        string line;
                        while ((line = input.ReadLine()) != null)
                        {
                            Request request;
                            try
                            {
                                request = new JavaScriptSerializer { MaxJsonLength = 1024 * 1024 }.Deserialize<Request>(line);
                                line = null;
                                if (request == null || request.id <= 0) throw new InvalidDataException();
                            }
                            catch
                            {
                                Write(new { id = 0, ok = false, code = "invalid_request" });
                                continue;
                            }
                            host.BeginInvoke(new Action(delegate { host.Execute(request); }));
                        }
                    }
                }
                catch (InvalidOperationException) { }
                catch (IOException) { }
                finally
                {
                    // Parent exit/crash closes the inherited pipe, including when
                    // no remote session has been added yet.
                    try { host.BeginInvoke(new Action(host.Close)); }
                    catch (InvalidOperationException) { }
                }
            });
            reader.Name = "mxterm-rdp-ipc";
            reader.IsBackground = true;
            reader.Start();
        }
    }

    internal sealed class Request
    {
        public long id;
        public string command;
        public string session_id;
        public RdpConfig config;
        public Bounds bounds;
        public WindowState window_state;
    }

    internal sealed class RdpConfig
    {
        public string title, host, username, domain, password, gateway_host;
        public int port, width, height, keyboard_hook_mode, audio_mode, authentication_level, gateway_mode, color_depth;
        public uint performance_flags;
        public bool dynamic_resize, use_multimon, clipboard, drives, printers, smart_cards, nla;
        public Appearance appearance;
    }

    internal sealed class Bounds { public int x, y, width, height; }
    internal sealed class WindowState { public int x, y, width, height; public bool maximized; }
    internal sealed class Appearance
    {
        public uint panel, text, muted, line, primary, danger;
        public uint? chrome, chrome_line, active, hover, success;
        public bool reduced_motion;
    }
}
