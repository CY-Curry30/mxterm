using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Windows.Forms;

namespace MXterm.RdpHost
{
    internal sealed class FeedbackPanel : Panel
    {
        private Color borderColor;
        private bool hasAppearance;
        private int radius;

        internal FeedbackPanel()
        {
            SetStyle(ControlStyles.UserPaint | ControlStyles.AllPaintingInWmPaint | ControlStyles.OptimizedDoubleBuffer, true);
        }

        internal void SetAppearance(Color background, Color border, int cornerRadius)
        {
            if (hasAppearance && BackColor == background && borderColor == border && radius == cornerRadius) return;
            BackColor = background;
            borderColor = border;
            radius = cornerRadius;
            hasAppearance = true;
            UpdateRegion();
            Invalidate();
        }

        protected override void OnSizeChanged(EventArgs e)
        {
            base.OnSizeChanged(e);
            UpdateRegion();
        }

        private void UpdateRegion()
        {
            // Keep the native panel rectangular for reliable child HWND clipping;
            // the rounded border is drawn in OnPaint without changing hit testing.
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            base.OnPaint(e);
            e.Graphics.SmoothingMode = SmoothingMode.AntiAlias;
            using (var path = RoundedPath(new Rectangle(0, 0, Width - 1, Height - 1), radius))
            using (var pen = new Pen(borderColor)) e.Graphics.DrawPath(pen, path);
        }

        private static GraphicsPath RoundedPath(Rectangle bounds, int radius)
        {
            var path = new GraphicsPath();
            int diameter = Math.Min(radius * 2, Math.Min(bounds.Width, bounds.Height));
            if (diameter <= 0) { path.AddRectangle(bounds); return path; }
            path.AddArc(bounds.Left, bounds.Top, diameter, diameter, 180, 90);
            path.AddArc(bounds.Right - diameter, bounds.Top, diameter, diameter, 270, 90);
            path.AddArc(bounds.Right - diameter, bounds.Bottom - diameter, diameter, diameter, 0, 90);
            path.AddArc(bounds.Left, bounds.Bottom - diameter, diameter, diameter, 90, 90);
            path.CloseFigure();
            return path;
        }
    }

    internal sealed class FeedbackMark : Control
    {
        private Color statusColor;
        internal Color StatusColor
        {
            get { return statusColor; }
            set { if (statusColor != value) { statusColor = value; Invalidate(); } }
        }

        internal FeedbackMark()
        {
            SetStyle(ControlStyles.UserPaint | ControlStyles.AllPaintingInWmPaint | ControlStyles.OptimizedDoubleBuffer, true);
            AccessibleRole = AccessibleRole.Indicator;
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            base.OnPaint(e);
            int diameter = Math.Min(Width, Height) - 2;
            if (diameter <= 0) return;
            var bounds = new Rectangle((Width - diameter) / 2, (Height - diameter) / 2, diameter, diameter);
            using (var brush = new SolidBrush(statusColor)) e.Graphics.FillEllipse(brush, bounds);
        }
    }

    internal sealed class Session : IDisposable
    {
        internal string Id;
        internal RdpAx Ax;
        internal object Client;
        internal RdpConfig Config;
        internal RdpEvents Events;
        internal Panel Feedback;
        internal FeedbackMark FeedbackMark;
        internal Label Message;
        internal Button Retry;
        internal string Phase = "connecting";
        internal bool Dialog, Parked;
        internal long? HiddenSince;
        internal long Started = Stopwatch.GetTimestamp();
        internal int LastWidth, LastHeight, ResizeRetries;
        internal uint LastDpi;

        public void Dispose()
        {
            // Unadvise before Disconnect; queued callbacks check session identity.
            if (Events != null) { Events.Dispose(); Events = null; }
            try { if (Client != null && Convert.ToInt32(Com.Get(Client, 6)) != 0) Com.Call(Client, 31); }
            catch (Exception) { }
            Client = null; // AxHost owns the RCW and releases its OLE site.
            if (Feedback != null) Feedback.Dispose();
            if (Ax != null) Ax.Dispose();
            Config = null;
        }
    }

    internal sealed class HostWindow : Form
    {
        private const int HiddenSessionParkDelaySeconds = 5;
        private readonly List<Session> sessions = new List<Session>();
        private readonly Timer maintenance = new Timer { Interval = 300 };
        private Session active;
        private bool allowShow, closing;
        private FormWindowState restoreWindowState = FormWindowState.Normal;
        private Appearance appearance;
        private readonly Icon appIcon;
        private readonly HostChrome chrome;

        internal int SessionCount { get { return sessions.Count; } }
        internal Session ActiveSession { get { return active; } }
        internal HostChrome Chrome { get { return chrome; } }
        private int Dip(int value) { return (int)Math.Round(value * Dpi / 96.0); }
        private uint Dpi { get { return IsHandleCreated ? Native.GetDpiForWindow(Handle) : 96; } }
        private int ChromeHeight { get { return Dip(HostChrome.HeightDip); } }
        private Color PanelColor { get { return appearance == null ? SystemColors.Control : Rgb(appearance.panel); } }
        private Color TextColor { get { return appearance == null ? SystemColors.ControlText : Rgb(appearance.text); } }
        private Color MutedColor { get { return appearance == null ? SystemColors.GrayText : Rgb(appearance.muted); } }
        private Color LineColor { get { return appearance == null ? SystemColors.ControlDark : Rgb(appearance.line); } }
        private static Color Rgb(uint colorRef) { return NativePalette.Decode(colorRef); }
        private static double SecondsSince(long start) { return (Stopwatch.GetTimestamp() - start) / (double)Stopwatch.Frequency; }

        internal HostWindow()
        {
            Text = "MXterm RDP";
            // The host is the native surface for a workbench pane, not a second
            // application: an unowned top-level Form would get its own taskbar
            // button (and Alt+Tab entry) next to the main window. The explicit
            // CreateParams flag below is authoritative; this property documents
            // the intent and keeps WinForms' own taskbar bookkeeping in sync.
            ShowInTaskbar = false;
            AutoScaleMode = AutoScaleMode.None;
            MinimumSize = new Size(720, 480);
            Size = new Size(1280, 860);
            StartPosition = FormStartPosition.CenterScreen;
            SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.UserPaint, true);
            appIcon = Icon.ExtractAssociatedIcon(Assembly.GetExecutingAssembly().Location);
            Icon = appIcon;
            Font = SystemFonts.MessageBoxFont;
            chrome = new HostChrome(Dip, appIcon, ActivateSession, CloseTab, delegate(int button)
            {
                if (button == 0) WindowState = FormWindowState.Minimized;
                else if (button == 1) WindowState = WindowState == FormWindowState.Maximized ? FormWindowState.Normal : FormWindowState.Maximized;
                else Close();
            }, delegate { FocusActiveSession(true); });
            Controls.Add(chrome);
            SynchronizeChrome();
            maintenance.Tick += delegate { MaintainSessions(); };
            maintenance.Start();
        }

        protected override CreateParams CreateParams
        {
            get
            {
                // WS_EX_TOOLWINDOW is the only style Win32 honours when keeping a
                // top-level window out of the taskbar and the Alt+Tab list. It is
                // applied at handle creation and survives handle recreation, so
                // the host can never flash a second taskbar button.
                CreateParams parameters = base.CreateParams;
                parameters.ExStyle |= Native.WsExToolWindow;
                return parameters;
            }
        }

        protected override void SetVisibleCore(bool value) { base.SetVisibleCore(value && allowShow); }

        internal void Execute(Request request)
        {
            try
            {
                switch (request.command)
                {
                    case "add":
                        if (request.config == null || String.IsNullOrEmpty(request.session_id)) throw new InvalidOperationException("invalid_add");
                        if (sessions.Count == 0) ApplyWindowState(request.window_state, request.bounds);
                        Session session = AddSession(request.session_id, request.config, true);
                        Protocol.Write(new { id = request.id, ok = true, @event = "added", session_id = session.Id,
                            hwnd = Handle.ToInt64(), control_hwnd = session.Ax.Handle.ToInt64() });
                        break;
                    case "close":
                        if (!CloseSession(request.session_id)) throw new InvalidOperationException("session_missing");
                        bool exiting = sessions.Count == 0;
                        if (exiting) Close();
                        Protocol.Write(new { id = request.id, ok = true, exiting = exiting });
                        break;
                    case "activate":
                        ActivateSession(FindSession(request.session_id));
                        Reveal();
                        Protocol.Write(new { id = request.id, ok = true });
                        break;
                    case "resize":
                        if (request.bounds == null) throw new InvalidOperationException("invalid_bounds");
                        Bounds = new Rectangle(request.bounds.x, request.bounds.y, Math.Max(720, request.bounds.width), Math.Max(480, request.bounds.height));
                        Protocol.Write(new { id = request.id, ok = true });
                        break;
                    case "shutdown":
                        Protocol.Write(new { id = request.id, ok = true });
                        Close();
                        break;
                    default: throw new InvalidOperationException("unknown_command");
                }
            }
            catch (Exception error)
            {
                // Never forward exception messages/COM descriptions: they can
                // contain server names, usernames or credential material.
                Protocol.Write(new { id = request.id, ok = false, code = "rdp_host_command_failed", hresult = error.InnerException == null ? error.HResult : error.InnerException.HResult });
                if (request.command == "add" && sessions.Count == 0) BeginInvoke(new Action(Close));
            }
            finally
            {
                if (request.config != null) request.config.password = null;
            }
        }

        private void ApplyWindowState(WindowState state, Bounds bounds)
        {
            if (state != null)
            {
                Rectangle restored = new Rectangle(state.x, state.y, Math.Max(720, state.width), Math.Max(480, state.height));
                Bounds = EnsureVisible(restored);
                StartPosition = FormStartPosition.Manual;
                restoreWindowState = state.maximized ? FormWindowState.Maximized : FormWindowState.Normal;
                WindowState = restoreWindowState;
            }
            else if (bounds != null)
            {
                Rectangle requested = new Rectangle(
                    bounds.x == Int32.MinValue ? Left : bounds.x,
                    bounds.y == Int32.MinValue ? Top : bounds.y,
                    Math.Max(720, bounds.width), Math.Max(480, bounds.height + ChromeHeight));
                Bounds = EnsureVisible(requested);
                StartPosition = FormStartPosition.Manual;
            }
        }

        internal static bool IsMeaningfullyVisible(Rectangle bounds)
        {
            foreach (Screen screen in Screen.AllScreens)
            {
                Rectangle visible = Rectangle.Intersect(screen.WorkingArea, bounds);
                if (visible.Width >= 160 && visible.Height >= 120) return true;
            }
            return false;
        }

        internal static Rectangle EnsureVisible(Rectangle bounds)
        {
            if (IsMeaningfullyVisible(bounds)) return bounds;
            Rectangle work = (Screen.PrimaryScreen ?? Screen.AllScreens[0]).WorkingArea;
            int x = work.Left + Math.Max(0, (work.Width - bounds.Width) / 2);
            int y = work.Top + Math.Max(0, (work.Height - bounds.Height) / 2);
            return new Rectangle(x, y, bounds.Width, bounds.Height);
        }

        internal static void ApplyMaximizedBounds(ref Native.MinMaxInfo info, Rectangle monitor, Rectangle work)
        {
            // WM_GETMINMAXINFO uses offsets from the monitor origin, not
            // absolute virtual-desktop coordinates. ptMaxPosition/ptMaxSize are
            // the final window rectangle for this borderless host: because
            // WM_NCCALCSIZE already removed the non-client area, Windows does
            // not add the resize frame around the request. Subtracting the
            // frame here left an 8 DIP gap on the left and top edges.
            info.MaxPosition.X = work.Left - monitor.Left;
            info.MaxPosition.Y = work.Top - monitor.Top;
            info.MaxSize.X = Math.Max(1, work.Width);
            info.MaxSize.Y = Math.Max(1, work.Height);
        }

        internal void Reveal()
        {
            allowShow = true;
            bool minimized = WindowState == FormWindowState.Minimized;
            Rectangle restored = minimized ? RestoreBounds : WindowState == FormWindowState.Normal ? Bounds : RestoreBounds;
            if (!IsMeaningfullyVisible(restored))
            {
                Bounds = EnsureVisible(restored);
                restoreWindowState = FormWindowState.Normal;
                WindowState = FormWindowState.Normal;
            }
            else if (minimized)
            {
                Bounds = restored;
                WindowState = restoreWindowState;
            }
            Show();
            Native.SetForegroundWindow(Handle);
            Activate();
            FocusActiveSession();
        }

        internal Session AddSession(string id, RdpConfig config, bool connect)
        {
            if (sessions.Exists(s => s.Id == id)) throw new InvalidOperationException("duplicate_session");
            if (config.width < 200 || config.height < 200 || config.keyboard_hook_mode < 0 || config.keyboard_hook_mode > 2)
                throw new InvalidOperationException("invalid_config");
            appearance = config.appearance;
            BackColor = PanelColor;
            allowShow = true;
            Show(); // The real top-level WinForms frame exists before the OLE site.
            PerformLayout();
            Session added = null;
            Exception last = null;
            foreach (string programId in new[] { "MsTscAx.MsTscAx.13", "MsTscAx.MsTscAx.12", "MsTscAx.MsTscAx.11", "MsTscAx.MsTscAx.10", "MsTscAx.MsTscAx.9" })
            {
                Type type = Type.GetTypeFromProgID(programId, false);
                if (type == null) continue;
                var ax = new RdpAx(type.GUID.ToString()) { Bounds = ContentBounds(), Visible = true };
                try
                {
                    ((ISupportInitialize)ax).BeginInit();
                    Controls.Add(ax);
                    ((ISupportInitialize)ax).EndInit();
                    ax.CreateControl();
                    object client = ax.Client;
                    Com.Configure(client, config, ax.Width, ax.Height, Dpi);
                    added = new Session { Id = id, Ax = ax, Client = client, Config = config };
                    CreateFeedback(added);
                    Subscribe(added);
                    break;
                }
                catch (Exception error)
                {
                    last = error;
                    if (added != null) { added.Dispose(); added = null; }
                    else ax.Dispose();
                }
            }
            if (added == null) throw new InvalidOperationException("rdp_control_configuration_failed", last);
            // No managed credential copy survives configuration. The control owns
            // the password needed for reconnect; retry never asks Rust to resend it.
            config.password = null;
            sessions.Add(added);
            ActivateSession(added);
            try { if (connect) Com.Call(added.Client, 30); }
            catch
            {
                sessions.Remove(added); active = null;
                SynchronizeChrome();
                added.Dispose();
                if (sessions.Count > 0) ActivateSession(sessions[sessions.Count - 1]);
                throw;
            }
            Reveal();
            return added;
        }

        private void Subscribe(Session session)
        {
            RdpEvents subscription = null;
            subscription = new RdpEvents(session.Client, delegate(string phase, int code)
            {
                // COM callbacks enqueue only. No control calls or byref decision
                // changes occur while MSTSC is on the stack.
                if (closing || !IsHandleCreated) return;
                BeginInvoke(new Action(delegate
                {
                    if (!sessions.Contains(session) || session.Events != subscription) return;
                    OnConnectionEvent(session, phase, code);
                }));
            });
            session.Events = subscription;
        }

        private void CreateFeedback(Session session)
        {
            var panel = new FeedbackPanel { Height = Dip(52), AccessibleName = "远程桌面连接状态" };
            var mark = new FeedbackMark { Size = new Size(Dip(9), Dip(9)), BackColor = PanelColor };
            var label = new Label { ForeColor = MutedColor, BackColor = Color.Transparent, AutoSize = false, TextAlign = ContentAlignment.MiddleLeft, AutoEllipsis = true };
            var retry = new Button { Text = "重试连接", AccessibleName = "重试连接", FlatStyle = FlatStyle.Flat, ForeColor = TextColor, BackColor = PanelColor, UseVisualStyleBackColor = false };
            retry.FlatAppearance.BorderColor = LineColor;
            retry.Click += delegate { Retry(session); };
            panel.Controls.Add(mark); panel.Controls.Add(label); panel.Controls.Add(retry); Controls.Add(panel);
            session.Feedback = panel; session.FeedbackMark = mark; session.Message = label; session.Retry = retry;
            UpdateFeedback(session);
        }

        private void ApplyFeedbackAppearance(Session session)
        {
            Appearance palette = session.Config == null ? null : session.Config.appearance;
            Color panel = palette == null ? SystemColors.Control : Rgb(palette.panel);
            Color text = palette == null ? SystemColors.ControlText : Rgb(palette.text);
            Color muted = palette == null ? SystemColors.GrayText : Rgb(palette.muted);
            Color line = palette == null ? SystemColors.ControlDark : Rgb(palette.line);
            session.Feedback.BackColor = panel;
            ((FeedbackPanel)session.Feedback).SetAppearance(panel, line, Dip(6));
            session.FeedbackMark.BackColor = panel;
            session.Message.ForeColor = text;
            session.Retry.ForeColor = text;
            session.Retry.BackColor = panel;
            session.Retry.FlatAppearance.BorderColor = line;
            Color accent = session.Phase == "fatal" || session.Phase == "disconnected"
                ? palette == null ? SystemColors.HotTrack : Rgb(palette.danger)
                : session.Phase == "login_complete"
                    ? palette == null || !palette.success.HasValue ? SystemColors.Highlight : Rgb(palette.success.Value)
                    : palette == null ? muted : Rgb(palette.primary);
            session.FeedbackMark.StatusColor = accent;
            session.FeedbackMark.Invalidate();
        }

        internal void OnConnectionEvent(Session session, string phase, int code)
        {
            if (phase == "dialog_open") session.Dialog = true;
            else if (phase == "dialog_close") session.Dialog = false;
            else
            {
                bool stopped = session.Phase == "disconnected" || session.Phase == "fatal";
                if ((phase == "connected" || phase == "connecting") && session.Phase == "login_complete") return;
                if (phase == "login_complete" && stopped) return;
                session.Phase = phase;
                if (phase == "login_complete") { session.ResizeRetries = 20; session.LastWidth = 0; }
                if (phase == "disconnected" || phase == "fatal")
                {
                    int extended = 0;
                    try { extended = Convert.ToInt32(Com.Get(session.Client, 103)); } catch { }
                    session.Message.Text = "连接已结束（代码 " + code + "，扩展代码 " + extended + "）";
                }
            }
            Protocol.Write(new { @event = "connection", session_id = session.Id, phase = phase, code = code });
            chrome.UpdateStatus(session);
            UpdateFeedback(session);
        }

        private void Retry(Session session)
        {
            if (session.Phase != "disconnected" && session.Phase != "fatal") return;
            if (session.Events != null) session.Events.Dispose();
            session.Events = null;
            try
            {
                Subscribe(session);
                session.Phase = "connecting"; session.Started = Stopwatch.GetTimestamp(); session.Dialog = false;
                chrome.UpdateStatus(session);
                UpdateFeedback(session);
                Com.Call(session.Client, 30);
                session.Ax.Focus();
            }
            catch (Exception error)
            {
                session.Phase = "fatal";
                chrome.UpdateStatus(session);
                session.Message.Text = "重试失败（代码 " + error.HResult + "）";
                UpdateFeedback(session);
            }
        }

        private void UpdateFeedback(Session session)
        {
            if (session.Feedback == null || session.Message == null || session.Retry == null) return;
            bool stopped = session.Phase == "disconnected" || session.Phase == "fatal";
            if (!stopped)
            {
                session.Message.Text = session.Phase == "connected" ? "已连接，等待远程登录…" :
                    session.Phase == "reconnecting" ? "正在重新连接…" : "正在连接远程桌面…";
                if (SecondsSince(session.Started) >= 20 && session.Phase != "login_complete")
                    session.Message.Text += "  请检查远程登录或身份验证提示。";
            }
            ApplyFeedbackAppearance(session);
            Rectangle desktop = ContentBounds();
            int width = Math.Min(Dip(560), Math.Max(Dip(320), desktop.Width - Dip(16)));
            int height = Dip(52);
            int left = desktop.Left + Math.Max(0, (desktop.Width - width) / 2);
            int top = desktop.Top + Dip(session.Phase == "connected" ? 44 : 8);
            session.Feedback.Bounds = new Rectangle(left, top, width, height);
            int trailing = stopped ? Dip(124) : Dip(14);
            session.Message.Bounds = new Rectangle(Dip(36), 0, Math.Max(Dip(100), width - Dip(36) - trailing), height);
            session.FeedbackMark.Location = new Point(Dip(16), (height - session.FeedbackMark.Height) / 2);
            session.Retry.Bounds = new Rectangle(width - Dip(112), (height - Dip(30)) / 2, Dip(100), Dip(30));
            session.Retry.Visible = stopped;
            session.Feedback.Visible = session == active && session.Phase != "login_complete" && !session.Dialog && WindowState != FormWindowState.Minimized;
            if (session.Feedback.Visible) session.Feedback.BringToFront();
        }

        private Session FindSession(string id)
        {
            Session session = sessions.Find(s => s.Id == id);
            if (session == null) throw new InvalidOperationException("session_missing");
            return session;
        }

        internal void ActivateSession(Session session)
        {
            active = session;
            appearance = session.Config.appearance;
            BackColor = PanelColor;
            Text = session.Config.title;
            foreach (Session other in sessions)
            {
                other.Ax.Visible = other == active;
                if (other != active && other.HiddenSince == null) other.HiddenSince = Stopwatch.GetTimestamp();
                UpdateFeedback(other);
            }
            session.HiddenSince = null;
            session.Ax.Bounds = ContentBounds();
            session.Ax.BringToFront();
            UpdateFeedback(session);
            session.ResizeRetries = 20;
            session.Parked = false;
            SynchronizeChrome();
            FocusActiveSession(true);
            Invalidate();
        }

        internal bool CloseSession(string id)
        {
            Session session = sessions.Find(s => s.Id == id);
            if (session == null) return false;
            int previousIndex = sessions.IndexOf(session);
            bool wasActive = session == active;
            sessions.Remove(session);
            if (wasActive) active = null;
            // Remove chrome references before COM teardown can reenter painting.
            if (!closing) SynchronizeChrome();
            session.Dispose();
            Protocol.Write(new { @event = "closed", session_id = id });
            if (wasActive && !closing)
            {
                if (sessions.Count > 0) ActivateSession(sessions[Math.Min(previousIndex, sessions.Count - 1)]);
            }
            Invalidate();
            return true;
        }

        internal void CloseTab(Session session)
        {
            if (!sessions.Contains(session)) return;
            CloseSession(session.Id);
            if (sessions.Count == 0) Close();
            else FocusActiveSession(true);
        }

        private void SynchronizeChrome()
        {
            if (chrome == null || closing) return;
            chrome.Bounds = new Rectangle(0, 0, ClientSize.Width, ChromeHeight);
            chrome.UpdateSessions(sessions, active, appearance, WindowState == FormWindowState.Maximized);
            chrome.BringToFront();
        }

        private void FocusActiveSession()
        {
            FocusActiveSession(false);
        }

        private void FocusActiveSession(bool explicitSwitch)
        {
            if (closing || active == null || !sessions.Contains(active) || active.Dialog) return;
            if (!explicitSwitch && (active.Retry.Focused || active.Feedback.ContainsFocus)) return;
            ActiveControl = active.Ax;
            active.Ax.Select();
            active.Ax.Focus();
        }

        protected override void OnActivated(EventArgs e) { base.OnActivated(e); FocusActiveSession(); }
        protected override void OnHandleCreated(EventArgs e)
        {
            base.OnHandleCreated(e);
            MinimumSize = new Size(Dip(720), Dip(480));
        }
        private Rectangle ContentBounds() { return new Rectangle(0, ChromeHeight, Math.Max(200, ClientSize.Width), Math.Max(200, ClientSize.Height - ChromeHeight)); }

        protected override void OnResize(EventArgs e)
        {
            base.OnResize(e);
            if (!IsHandleCreated || sessions == null) return;
            if (WindowState != FormWindowState.Minimized) restoreWindowState = WindowState;
            SynchronizeChrome();
            foreach (Session session in sessions)
            {
                if (WindowState == FormWindowState.Minimized)
                { if (session.HiddenSince == null) session.HiddenSince = Stopwatch.GetTimestamp(); }
                else if (session == active)
                {
                    session.HiddenSince = null;
                    session.Ax.Bounds = ContentBounds();
                    session.ResizeRetries = 20;
                }
                UpdateFeedback(session);
            }
            Invalidate();
        }

        private void MaintainSessions()
        {
            foreach (Session session in sessions)
            {
                bool visible = session == active && WindowState != FormWindowState.Minimized;
                if (visible)
                {
                    session.HiddenSince = null;
                    if (session.Phase == "login_complete" && session.Config.dynamic_resize && session.ResizeRetries > 0)
                    {
                        session.ResizeRetries--;
                        SyncDisplay(session, session.Ax.Width, session.Ax.Height, Dpi, false);
                    }
                }
                else if (ShouldParkSession(session, Stopwatch.GetTimestamp()))
                    SyncDisplay(session, 640, 480, 96, true);
                if (session.Phase != "login_complete") UpdateFeedback(session);
            }
        }

        internal bool ShouldParkSession(Session session, long now)
        {
            return session.Config != null && session.Config.dynamic_resize && session.Phase == "login_complete"
                && session.HiddenSince.HasValue && !session.Parked
                && (now - session.HiddenSince.Value) / (double)Stopwatch.Frequency >= HiddenSessionParkDelaySeconds;
        }

        private static void SyncDisplay(Session session, int width, int height, uint dpi, bool park)
        {
            if (width < 200 || height < 200) return;
            if (width == session.LastWidth && height == session.LastHeight && dpi == session.LastDpi && session.Parked == park) return;
            try
            {
                uint scale = Math.Max(100, Math.Min(500, dpi * 100 / 96));
                session.Client.GetType().InvokeMember("UpdateSessionDisplaySettings", BindingFlags.InvokeMethod,
                    null, session.Client, new object[] { (uint)width, (uint)height, (uint)0, (uint)0, (uint)0, scale, (uint)100 }, System.Globalization.CultureInfo.InvariantCulture);
                session.LastWidth = width; session.LastHeight = height; session.LastDpi = dpi; session.Parked = park;
            }
            catch { /* Servers can reject display updates before their desktop is ready; bounded retries continue. */ }
        }

        protected override void OnFormClosing(FormClosingEventArgs e)
        {
            closing = true;
            maintenance.Stop();
            chrome.Hide();
            bool maximized = WindowState == FormWindowState.Maximized || restoreWindowState == FormWindowState.Maximized;
            Rectangle normal = WindowState == FormWindowState.Normal ? Bounds : RestoreBounds;
            normal = EnsureVisible(normal);
            Protocol.Write(new { @event = "window_state", state = new WindowState { x = normal.X, y = normal.Y,
                width = normal.Width, height = normal.Height, maximized = maximized } });
            foreach (Session session in sessions.ToArray()) CloseSession(session.Id);
            base.OnFormClosing(e);
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) { maintenance.Dispose(); if (appIcon != null) appIcon.Dispose(); }
            base.Dispose(disposing);
        }

        protected override void WndProc(ref Message message)
        {
            const int WM_GETMINMAXINFO = 0x24, WM_NCCALCSIZE = 0x83, WM_NCHITTEST = 0x84, WM_DPICHANGED = 0x2e0;
            if (message.Msg == WM_NCCALCSIZE && message.WParam != IntPtr.Zero)
            {
                // Keep the maximized client area inside the work area. Windows
                // grows the window rectangle by the resize frame on every edge
                // once it fills the work area; that frame stays off-screen, but
                // its counterpart must not push the remote desktop past it.
                if (Native.IsZoomed(Handle))
                {
                    var parameters = (Native.NcCalcSizeParams)Marshal.PtrToStructure(message.LParam, typeof(Native.NcCalcSizeParams));
                    var proposed = Rectangle.FromLTRB(parameters.Rect0.Left, parameters.Rect0.Top, parameters.Rect0.Right, parameters.Rect0.Bottom);
                    // Judge the target monitor from the proposed client rect:
                    // during a maximize across monitors the window handle can
                    // still report the previous screen.
                    Rectangle fitted = Rectangle.Intersect(proposed, Screen.FromRectangle(proposed).WorkingArea);
                    if (fitted != proposed && fitted.Width > 0 && fitted.Height > 0)
                    {
                        parameters.Rect0.Left = fitted.Left; parameters.Rect0.Top = fitted.Top;
                        parameters.Rect0.Right = fitted.Right; parameters.Rect0.Bottom = fitted.Bottom;
                        Marshal.StructureToPtr(parameters, message.LParam, false);
                    }
                }
                message.Result = IntPtr.Zero;
                return;
            }
            base.WndProc(ref message);
            if (message.Msg == WM_GETMINMAXINFO)
            {
                Screen screen = Screen.FromHandle(Handle);
                var info = (Native.MinMaxInfo)Marshal.PtrToStructure(message.LParam, typeof(Native.MinMaxInfo));
                ApplyMaximizedBounds(ref info, screen.Bounds, screen.WorkingArea);
                Marshal.StructureToPtr(info, message.LParam, false);
                message.Result = IntPtr.Zero;
            }
            else if (message.Msg == WM_NCHITTEST)
            {
                long pos = message.LParam.ToInt64();
                Point point = PointToClient(new Point((short)(pos & 0xffff), (short)((pos >> 16) & 0xffff)));
                int edge = Dip(6), hit = 1;
                if (WindowState != FormWindowState.Maximized)
                {
                    bool left = point.X < edge, right = point.X >= ClientSize.Width - edge;
                    if (point.Y < edge) hit = left ? 13 : right ? 14 : 12;
                    else if (point.Y >= ClientSize.Height - edge) hit = left ? 16 : right ? 17 : 15;
                    else if (left) hit = 10; else if (right) hit = 11;
                }
                if (hit == 1 && point.Y < ChromeHeight) hit = 2;
                message.Result = new IntPtr(hit);
            }
            else if (message.Msg == WM_DPICHANGED)
            {
                var suggested = (Native.WindowRect)Marshal.PtrToStructure(message.LParam, typeof(Native.WindowRect));
                MinimumSize = new Size(Dip(720), Dip(480));
                Bounds = Rectangle.FromLTRB(suggested.Left, suggested.Top, suggested.Right, suggested.Bottom);
                SynchronizeChrome();
                foreach (Session session in sessions) { session.Ax.Bounds = ContentBounds(); session.ResizeRetries = 20; UpdateFeedback(session); }
                Invalidate();
            }
        }
    }
}
