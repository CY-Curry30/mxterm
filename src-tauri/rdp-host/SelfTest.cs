using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Reflection;
using System.Windows.Forms;

namespace MXterm.RdpHost
{
    internal static class SelfTest
    {
        private static string failedCheck;
        private static void Check(bool condition, string stage)
        {
            if (!condition) { failedCheck = stage; throw new InvalidOperationException(); }
        }

        private static void CheckActive(HostWindow host, Session session, IList<Session> sessions)
        {
            Check(host.ActiveSession == session, "active_identity");
            Check(host.Chrome.VisibleSessions.Contains(session), "active_visible");
            Check(session.Ax.ContainsFocus, "active_focus");
            foreach (Session other in sessions)
                if (other != session && other.Config != null) Check(!other.Ax.Visible && !other.Feedback.Visible, "inactive_visibility");
        }

        private static uint ColorRef(string html)
        {
            Color value = ColorTranslator.FromHtml(html);
            return (uint)(value.R | value.G << 8 | value.B << 16);
        }

        // Token-default fixtures exercise both native palettes without a WebView.
        private static Appearance Palette(bool dark)
        {
            return new Appearance {
                panel = ColorRef(dark ? "#171b21" : "#ffffff"), text = ColorRef(dark ? "#e6e9ee" : "#20242a"),
                muted = ColorRef(dark ? "#a4adb9" : "#626b78"), line = ColorRef(dark ? "#2a3039" : "#dde1e7"),
                primary = ColorRef(dark ? "#73b7ff" : "#2374c6"), danger = ColorRef(dark ? "#ff8f9b" : "#dc2626"),
                chrome = ColorRef(dark ? "#202022" : "#f2f4f7"), chrome_line = ColorRef(dark ? "#363638" : "#dae0e7"),
                active = ColorRef(dark ? "#171b21" : "#ffffff"), hover = ColorRef(dark ? "#2e3237" : "#f1f1f1"),
                success = ColorRef(dark ? "#57d8ae" : "#15803d")
            };
        }

        private static void CheckLayouts(IList<Session> sessions, Session active)
        {
            Check(NativePalette.Decode(0x563412).ToArgb() == Color.FromArgb(0x12, 0x34, 0x56).ToArgb(), "colorref");
            string output = Environment.GetEnvironmentVariable("MXTERM_RDP_PREVIEW_DIR");
            if (!String.IsNullOrEmpty(output)) Directory.CreateDirectory(output);
            foreach (bool dark in new[] { false, true })
                foreach (int dpi in new[] { 96, 144, 192 })
                    foreach (int width in new[] { 720, 1280 })
                    {
                        Func<int, int> scale = value => (int)Math.Round(value * dpi / 96.0);
                        using (var chrome = new HostChrome(scale, null, delegate { }, delegate { }, delegate { }, delegate { }))
                        {
                            chrome.Size = new Size(scale(width), scale(40));
                            chrome.UpdateSessions(sessions, active, Palette(dark), false);
                            Check(chrome.VisibleSessions.Count <= 5 && chrome.VisibleSessions.Contains(active), "overflow_active_visible");
                            Check(chrome.VisibleSessions.Count + chrome.OverflowSessions.Count == sessions.Count, "overflow_session_count");
                            int previousRight = 0;
                            foreach (Session session in chrome.VisibleSessions)
                            {
                                Rectangle bounds = chrome.SelectButton(session).Bounds;
                                Check(bounds.Left >= previousRight && bounds.Right <= scale(width - 138 - 24), "tab_caption_bounds");
                                previousRight = bounds.Right;
                            }
                            if (!String.IsNullOrEmpty(output))
                                using (var bitmap = new Bitmap(chrome.Width, chrome.Height))
                                {
                                    chrome.DrawToBitmap(bitmap, chrome.ClientRectangle);
                                    bitmap.Save(Path.Combine(output, (dark ? "dark" : "light") + "-" + dpi + "-" + width + ".png"), ImageFormat.Png);
                                }
                        }
                    }
        }

        internal static int Run()
        {
            int result = 1;
            string stage = "initialization";
            using (var host = new HostWindow())
            {
                IntPtr handle = host.Handle;
                host.BeginInvoke(new Action(delegate
                {
                    try
                    {
                        stage = "configuration";
                        var sessions = new List<Session>();
                        for (int index = 0; index < 8; index++)
                        {
                            int mode = index % 3;
                            var config = new RdpConfig { host = "192.0.2.1", username = "example", title = "MXterm RDP - 远程桌面 " + (index + 1),
                                port = 3389, width = 1024, height = 768, color_depth = 32, keyboard_hook_mode = mode,
                                dynamic_resize = true, nla = true, password = "fixture", appearance = Palette(false) };
                            Session session = host.AddSession("test-" + index, config, false);
                            sessions.Add(session);
                            Check(config.password == null && session.Ax.Parent == host && session.Events != null, "control_lifecycle");
                            Check(Convert.ToInt32(Com.Property(Com.Get(session.Client, 102), "KeyboardHookMode")) == mode, "keyboard_mode");
                        }
                        Session active = host.ActiveSession;
                        CheckActive(host, active, sessions);
                        stage = "scale";
                        foreach (uint dpi in new uint[] { 96, 144, 192 })
                        {
                            Com.SetScale(active.Client, dpi);
                            Check(Com.GetScale(active.Client, "DesktopScaleFactor") == dpi * 100 / 96 && Com.GetScale(active.Client, "DeviceScaleFactor") == 100, "scale_readback");
                        }
                        stage = "feedback";
                        host.OnConnectionEvent(active, "connected", 0);
                        Check(active.Feedback.Visible, "pending_feedback");
                        host.OnConnectionEvent(active, "dialog_open", 0);
                        Check(!active.Feedback.Visible, "dialog_feedback");
                        host.OnConnectionEvent(active, "dialog_close", 0);
                        host.OnConnectionEvent(active, "login_complete", 0);
                        Check(!active.Feedback.Visible, "login_feedback");
                        host.OnConnectionEvent(active, "connected", 0);
                        Check(active.Phase == "login_complete", "late_connected");
                        host.OnConnectionEvent(active, "disconnected", 1);
                        host.OnConnectionEvent(active, "login_complete", 0);
                        Check(active.Feedback.Visible && active.Retry.Visible && active.Phase == "disconnected", "disconnect_feedback");

                        stage = "tab_switch";
                        host.Chrome.SelectButton(sessions[0]).PerformClick();
                        CheckActive(host, sessions[0], sessions);
                        stage = "overflow_switch";
                        Check(host.Chrome.OverflowSessions.Count > 0, "overflow_required");
                        Session hidden = host.Chrome.OverflowSessions[host.Chrome.OverflowSessions.Count - 1];
                        host.Chrome.OpenOverflow();
                        ToolStripMenuItem target = null;
                        foreach (ToolStripMenuItem item in host.Chrome.OverflowMenu.Items) if (item.Tag == hidden) target = item;
                        Check(target != null, "overflow_item"); target.PerformClick();
                        // Menu dismissal restores desktop focus through BeginInvoke.
                        Application.DoEvents();
                        CheckActive(host, hidden, sessions);

                        stage = "layout_palettes";
                        CheckLayouts(sessions, hidden);
                        stage = "close_inactive";
                        host.Chrome.CloseButton(sessions[0]).PerformClick();
                        Check(host.SessionCount == 7 && sessions[0].Ax.IsDisposed, "inactive_dispose");
                        CheckActive(host, hidden, sessions);
                        stage = "close_active";
                        host.Chrome.CloseButton(hidden).PerformClick();
                        Check(host.SessionCount == 6 && hidden.Ax.IsDisposed, "active_dispose");
                        CheckActive(host, sessions[6], sessions);
                        stage = "middle_close";
                        // Dispatch the control's own mouse event; no system input injection.
                        typeof(Control).GetMethod("OnMouseUp", BindingFlags.Instance | BindingFlags.NonPublic).Invoke(
                            host.Chrome.SelectButton(sessions[6]), new object[] { new MouseEventArgs(MouseButtons.Middle, 1, 10, 10, 0) });
                        Check(host.SessionCount == 5 && sessions[6].Ax.IsDisposed, "middle_dispose");
                        CheckActive(host, sessions[5], sessions);
                        stage = "final_cleanup";
                        for (int index = 1; index < 5; index++) host.CloseSession(sessions[index].Id);
                        Check(host.SessionCount == 1 && host.ActiveSession == sessions[5], "remaining_session");
                        host.Execute(new Request { id = 1, command = "close", session_id = sessions[5].Id });
                        Check(host.IsDisposed && host.SessionCount == 0, "last_close");
                        Protocol.Write(new { @event = "self_test", ok = true }); result = 0;
                    }
                    catch (Exception error)
                    {
                        Protocol.Write(new { @event = "self_test", ok = false, hresult = error.HResult, stage = stage, check = failedCheck,
                            inner_hresult = error.InnerException == null ? 0 : error.InnerException.HResult });
                    }
                    finally { host.Close(); }
                }));
                Application.Run(host);
            }
            return result;
        }
    }
}
