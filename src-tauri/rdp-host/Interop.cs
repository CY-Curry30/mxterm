using System;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Windows.Forms;

namespace MXterm.RdpHost
{
    // AxHost owns the entire OLE site, activation and accelerator contract. Do not
    // override its keyboard preprocessing or reparent this control across processes.
    internal sealed class RdpAx : AxHost
    {
        internal RdpAx(string clsid) : base(clsid) { TabStop = true; }
        internal object Client { get { return GetOcx(); } }
    }

    internal static class Com
    {
        internal static object Get(object target, int id)
        {
            return target.GetType().InvokeMember("[DispID=" + id + "]",
                BindingFlags.GetProperty, null, target, null, CultureInfo.InvariantCulture);
        }

        internal static void Set(object target, int id, object value)
        {
            target.GetType().InvokeMember("[DispID=" + id + "]",
                BindingFlags.SetProperty, null, target, new object[] { value }, CultureInfo.InvariantCulture);
        }

        internal static void Call(object target, int id)
        {
            target.GetType().InvokeMember("[DispID=" + id + "]",
                BindingFlags.InvokeMethod, null, target, null, CultureInfo.InvariantCulture);
        }

        internal static object Property(object target, string name)
        {
            return target.GetType().InvokeMember(name, BindingFlags.GetProperty,
                null, target, null, CultureInfo.InvariantCulture);
        }

        internal static void Property(object target, string name, object value)
        {
            target.GetType().InvokeMember(name, BindingFlags.SetProperty,
                null, target, new object[] { value }, CultureInfo.InvariantCulture);
        }

        internal static void Configure(object client, RdpConfig config, int width, int height, uint dpi)
        {
            Set(client, 1, config.host);
            Set(client, 3, config.username);
            if (!String.IsNullOrEmpty(config.domain)) Set(client, 2, config.domain);
            Set(client, 12, config.dynamic_resize ? width : config.width);
            Set(client, 13, config.dynamic_resize ? height : config.height);
            Set(client, 100, config.color_depth);
            object advanced = Get(client, 701); // IMsRdpClient9.AdvancedSettings9
            Set(advanced, 108, config.port);
            Property(advanced, "EnableWindowsKey", config.keyboard_hook_mode == 0 ? 0 : 1);
            Property(advanced, "GrabFocusOnConnect", true);
            Property(advanced, "SmartSizing", config.dynamic_resize);
            Property(advanced, "EnableAutoReconnect", true);
            Property(advanced, "EnableCredSspSupport", config.nla);
            Property(advanced, "AuthenticationLevel", (uint)config.authentication_level);
            Property(advanced, "RedirectClipboard", config.clipboard);
            Property(advanced, "RedirectDrives", config.drives);
            Property(advanced, "RedirectPrinters", config.printers);
            Property(advanced, "RedirectSmartCards", config.smart_cards);
            Property(advanced, "AudioRedirectionMode", (uint)config.audio_mode);
            Property(advanced, "PerformanceFlags", config.performance_flags);
            object secured = Get(client, 102); // IMsRdpClient.SecuredSettings2
            Property(secured, "KeyboardHookMode", config.keyboard_hook_mode);
            if (Convert.ToInt32(Property(secured, "KeyboardHookMode")) != config.keyboard_hook_mode)
                throw new InvalidOperationException("keyboard_hook_not_confirmed");

            if (!String.IsNullOrEmpty(config.gateway_host))
            {
                object transport = Get(client, 800);
                Property(transport, "GatewayHostname", config.gateway_host);
                Property(transport, "GatewayUsageMethod", (uint)config.gateway_mode);
                Property(transport, "GatewayCredsSource", (uint)4);
                Property(transport, "GatewayProfileUsageMethod", (uint)1);
            }
            SetScale(client, dpi);
            if (config.use_multimon) SetFlag(client, NonScriptable5, 53, true);
            if (config.password != null)
            {
                SetFlag(client, NonScriptable3, 19, false); // PromptForCredentials
                SetFlag(client, NonScriptable3, 23, config.nla);
                SetFlag(client, NonScriptable3, 31, false); // WarnAboutSendingCredentials
                SetFlag(client, NonScriptable4, 47, false); // PromptForCredsOnClient
                SetFlag(client, NonScriptable4, 45, false); // AllowCredentialSaving
                SetFlag(client, NonScriptable5, 63, false); // AllowPromptingForCredentials
                ((IPassword)client).SetPassword(config.password);
            }
        }

        internal static void SetScale(object client, uint dpi)
        {
            uint scale = Math.Max(100, Math.Min(500, dpi * 100 / 96));
            IExtendedSettings settings = (IExtendedSettings)client;
            object desktop = scale, device = (uint)100;
            settings.PutProperty("DesktopScaleFactor", ref desktop);
            settings.PutProperty("DeviceScaleFactor", ref device);
        }

        internal static uint GetScale(object client, string name)
        {
            object value;
            ((IExtendedSettings)client).GetProperty(name, out value);
            return Convert.ToUInt32(value);
        }

        // NS3 includes both RedirectDynamicDrives and RedirectDynamicDevices
        // before DeviceCollection/DriveCollection. Omitting either shifts every
        // later slot. NS3 ends at 36, NS4 at 52, NS5 credentials setter is 63.
        private static readonly Guid NonScriptable3 = new Guid("b3378d90-0728-45c7-8ed7-b6159fb92219");
        private static readonly Guid NonScriptable4 = new Guid("f50fa8aa-1c7d-4f59-b15c-a90cacae1fcb");
        private static readonly Guid NonScriptable5 = new Guid("4f6996d5-d7b1-412c-b0ff-063718566907");

        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int SetBoolean(IntPtr self, short value);

        // Stable vtable slots from the MSTSC NonScriptable ABI, including IUnknown.
        // These are not IDispatch properties. Always release both queried pointers.
        private static void SetFlag(object client, Guid iid, int slot, bool value)
        {
            IntPtr unknown = Marshal.GetIUnknownForObject(client), pointer = IntPtr.Zero;
            try
            {
                Marshal.ThrowExceptionForHR(Marshal.QueryInterface(unknown, ref iid, out pointer));
                IntPtr function = Marshal.ReadIntPtr(Marshal.ReadIntPtr(pointer), slot * IntPtr.Size);
                var setter = (SetBoolean)Marshal.GetDelegateForFunctionPointer(function, typeof(SetBoolean));
                Marshal.ThrowExceptionForHR(setter(pointer, value ? (short)-1 : (short)0));
            }
            finally
            {
                if (pointer != IntPtr.Zero) Marshal.Release(pointer);
                Marshal.Release(unknown);
            }
        }

        [ComImport, Guid("c1e6743a-41c1-4a74-832a-0dd06c1c7a0e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IPassword
        {
            void SetPassword([MarshalAs(UnmanagedType.BStr)] string password);
        }

        [ComImport, Guid("302d8188-0052-4807-806a-362b628f9ac5"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IExtendedSettings
        {
            void PutProperty([MarshalAs(UnmanagedType.BStr)] string name, [In, MarshalAs(UnmanagedType.Struct)] ref object value);
            void GetProperty([MarshalAs(UnmanagedType.BStr)] string name, [Out, MarshalAs(UnmanagedType.Struct)] out object value);
        }
    }

    [ComVisible(true), Guid("336d5562-efa8-482e-8cb3-c5c0fc7a7db6"), InterfaceType(ComInterfaceType.InterfaceIsIDispatch)]
    public interface IRdpEvents
    {
        [DispId(1)] void Connecting();
        [DispId(2)] void Connected();
        [DispId(3)] void LoginComplete();
        [DispId(4)] void Disconnected(int reason);
        [DispId(10)] void Fatal(int code);
        [DispId(17)] void AutoReconnecting(int reason, int attempt, [In, Out] ref int decision);
        [DispId(18)] void DialogDisplaying();
        [DispId(19)] void DialogDismissed();
        [DispId(33)] void AutoReconnected();
        [DispId(34)] void AutoReconnecting2(int reason, [MarshalAs(UnmanagedType.VariantBool)] bool networkAvailable, int attempt, int maxAttempts);
    }

    [ComVisible(true), ClassInterface(ClassInterfaceType.None)]
    public sealed class RdpEvents : IRdpEvents, IDisposable
    {
        private IConnectionPoint point;
        private int cookie;
        private readonly Action<string, int> enqueue;
        internal RdpEvents(object client, Action<string, int> enqueue)
        {
            this.enqueue = enqueue;
            Guid iid = typeof(IRdpEvents).GUID;
            ((IConnectionPointContainer)client).FindConnectionPoint(ref iid, out point);
            point.Advise(this, out cookie);
        }
        public void Connecting() { enqueue("connecting", 0); }
        public void Connected() { enqueue("connected", 0); }
        public void LoginComplete() { enqueue("login_complete", 0); }
        public void Disconnected(int reason) { enqueue("disconnected", reason); }
        public void Fatal(int code) { enqueue("fatal", code); }
        public void AutoReconnecting(int reason, int attempt, ref int decision) { enqueue("reconnecting", reason); }
        public void DialogDisplaying() { enqueue("dialog_open", 0); }
        public void DialogDismissed() { enqueue("dialog_close", 0); }
        public void AutoReconnected() { enqueue("login_complete", 0); }
        public void AutoReconnecting2(int reason, bool networkAvailable, int attempt, int maxAttempts) { enqueue("reconnecting", reason); }
        public void Dispose()
        {
            if (point == null) return;
            try { point.Unadvise(cookie); }
            catch (COMException) { }
            finally { Marshal.ReleaseComObject(point); point = null; }
        }
    }

    internal static class Native
    {
        [StructLayout(LayoutKind.Sequential)]
        internal struct WindowRect { internal int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)]
        internal struct WindowPoint { internal int X, Y; }
        [StructLayout(LayoutKind.Sequential)]
        internal struct MinMaxInfo
        {
            internal WindowPoint Reserved, MaxSize, MaxPosition, MinTrackSize, MaxTrackSize;
        }
        [StructLayout(LayoutKind.Sequential)]
        internal struct NcCalcSizeParams
        {
            internal WindowRect Rect0, Rect1, Rect2;
            internal IntPtr WindowPos;
        }
        internal const int ExtendedStyleIndex = -20;
        internal const int WsExToolWindow = 0x00000080;

        [DllImport("user32.dll")] internal static extern uint GetDpiForWindow(IntPtr hwnd);
        [DllImport("user32.dll")] internal static extern bool IsZoomed(IntPtr hwnd);
        [DllImport("user32.dll")] internal static extern IntPtr GetFocus();
        [DllImport("user32.dll")] internal static extern bool IsChild(IntPtr parent, IntPtr child);
        [DllImport("user32.dll")] internal static extern bool SetForegroundWindow(IntPtr hwnd);
        [DllImport("user32.dll")] internal static extern bool ShowWindow(IntPtr hwnd, int command);
        [DllImport("user32.dll")] internal static extern bool ReleaseCapture();
        [DllImport("user32.dll")] internal static extern IntPtr SendMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam);
        [DllImport("dwmapi.dll")] internal static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

        [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
        private static extern int GetWindowLong32(IntPtr hwnd, int index);
        [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
        private static extern IntPtr GetWindowLongPtr64(IntPtr hwnd, int index);

        // GetWindowLongPtrW has no 32-bit export; the x86 build must use the
        // 32-bit entry point through the same call site.
        internal static long GetExtendedStyle(IntPtr hwnd)
        {
            return IntPtr.Size == 8
                ? GetWindowLongPtr64(hwnd, ExtendedStyleIndex).ToInt64()
                : (uint)GetWindowLong32(hwnd, ExtendedStyleIndex);
        }

        [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern int SetCurrentProcessExplicitAppUserModelID(
            [MarshalAs(UnmanagedType.LPWStr)] string appId);
        [DllImport("shell32.dll", ExactSpelling = true)]
        private static extern int GetCurrentProcessExplicitAppUserModelID(out IntPtr appId);

        internal static bool SetAppUserModelId(string appId)
        {
            return !String.IsNullOrEmpty(appId) && SetCurrentProcessExplicitAppUserModelID(appId) >= 0;
        }

        internal static string GetProcessAppUserModelId()
        {
            IntPtr raw;
            if (GetCurrentProcessExplicitAppUserModelID(out raw) < 0 || raw == IntPtr.Zero) return null;
            try { return Marshal.PtrToStringUni(raw); }
            finally { Marshal.FreeCoTaskMem(raw); }
        }
    }
}
