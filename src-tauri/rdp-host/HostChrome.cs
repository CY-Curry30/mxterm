using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Windows.Forms;

namespace MXterm.RdpHost
{
    // The native HWND surface cannot contain Radix controls. Keep its palette on
    // the shared app-token bridge and use Windows buttons/menus for accessibility.
    internal sealed class NativePalette
    {
        internal Color Panel, Chrome, Line, Text, Muted, Primary, Danger, Active, Hover, Success;
        // The bridge uses Windows COLORREF (0x00BBGGRR), not HTML 0xRRGGBB.
        internal static Color Decode(uint value) { return Color.FromArgb((int)value & 255, (int)(value >> 8) & 255, (int)(value >> 16) & 255); }
        internal NativePalette(Appearance value)
        {
            Panel = value == null ? SystemColors.Control : Decode(value.panel);
            Chrome = value == null || !value.chrome.HasValue ? Panel : Decode(value.chrome.Value);
            Line = value == null ? SystemColors.ControlDark : Decode(value.chrome_line ?? value.line);
            Text = value == null ? SystemColors.ControlText : Decode(value.text);
            Muted = value == null ? SystemColors.GrayText : Decode(value.muted);
            Primary = value == null ? SystemColors.Highlight : Decode(value.primary);
            Danger = value == null ? SystemColors.HotTrack : Decode(value.danger);
            Active = value == null || !value.active.HasValue ? Panel : Decode(value.active.Value);
            Hover = value == null ? SystemColors.ControlLight : Decode(value.hover ?? value.active ?? value.panel);
            Success = value == null || !value.success.HasValue ? Primary : Decode(value.success.Value);
        }
    }

    internal enum ChromeGlyph { None, Close, Minimize, Maximize, Restore, Overflow }

    internal sealed class ChromeButton : Button
    {
        internal NativePalette Palette;
        internal ChromeGlyph Glyph;
        internal Session Session;
        internal bool Selected, Caption;
        internal Func<int, int> ScaleDip;
        private bool hot;

        internal ChromeButton()
        {
            FlatStyle = FlatStyle.Flat;
            FlatAppearance.BorderSize = 0;
            Cursor = Cursors.Hand;
            SetStyle(ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.AllPaintingInWmPaint, true);
        }

        private int Dip(int value) { return ScaleDip == null ? value : ScaleDip(value); }
        protected override void OnMouseEnter(EventArgs e) { hot = true; base.OnMouseEnter(e); Invalidate(true); }
        protected override void OnMouseLeave(EventArgs e) { hot = false; base.OnMouseLeave(e); Invalidate(true); }
        protected override void OnGotFocus(EventArgs e) { base.OnGotFocus(e); Invalidate(true); }
        protected override void OnLostFocus(EventArgs e) { base.OnLostFocus(e); Invalidate(true); }
        private Color SurfaceColor { get { return Caption && Glyph == ChromeGlyph.Close && hot ? Palette.Danger : hot || Focused ? Palette.Hover : Selected ? Palette.Active : BackColor; } }

        protected override void OnPaint(PaintEventArgs e)
        {
            if (Palette == null) return;
            var colors = Palette;
            Graphics g = e.Graphics;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            var parentButton = Parent as ChromeButton;
            g.Clear(parentButton == null ? BackColor : parentButton.SurfaceColor);
            bool danger = Caption && Glyph == ChromeGlyph.Close && hot;
            Color foreground = danger ? colors.Panel : Selected || hot ? colors.Text : colors.Muted;
            Rectangle rect = new Rectangle(0, 0, Math.Max(1, Width - 1), Math.Max(1, Height - 1));
            using (GraphicsPath shape = Rounded(rect, Caption ? 0 : Dip(6)))
            {
                if (hot || Selected || Focused)
                    using (var brush = new SolidBrush(danger ? colors.Danger : hot || Focused ? colors.Hover : colors.Active)) g.FillPath(brush, shape);
                if (Selected) using (var pen = new Pen(colors.Line)) g.DrawPath(pen, shape);
            }
            if (Session != null)
            {
                bool ready = Session.Phase == "login_complete", stopped = Session.Phase == "fatal" || Session.Phase == "disconnected";
                var dot = new Rectangle(Dip(10), Height / 2 - Dip(3), Dip(6), Dip(6));
                using (var brush = new SolidBrush(stopped ? colors.Danger : ready ? colors.Success : colors.Primary))
                    if (ready || stopped) g.FillEllipse(brush, dot); else using (var pen = new Pen(brush, Math.Max(1, Dip(1)))) g.DrawEllipse(pen, dot);
                TextRenderer.DrawText(g, HostChrome.Label(Session), Font, new Rectangle(Dip(23), 0, Math.Max(0, Width - Dip(49)), Height), foreground,
                    TextFormatFlags.VerticalCenter | TextFormatFlags.EndEllipsis | TextFormatFlags.NoPrefix | TextFormatFlags.SingleLine);
                if (Selected) using (var pen = new Pen(colors.Primary, Math.Max(1, Dip(2)))) g.DrawLine(pen, Dip(10), Height - Dip(2), Width - Dip(10), Height - Dip(2));
            }
            else
            {
                float x = Width / 2f, y = Height / 2f, unit = Dip(4);
                using (var pen = new Pen(foreground, Math.Max(1, Dip(1))))
                {
                    if (Glyph == ChromeGlyph.Close) { g.DrawLine(pen, x - unit, y - unit, x + unit, y + unit); g.DrawLine(pen, x + unit, y - unit, x - unit, y + unit); }
                    else if (Glyph == ChromeGlyph.Minimize) g.DrawLine(pen, x - unit, y, x + unit, y);
                    else if (Glyph == ChromeGlyph.Maximize) g.DrawRectangle(pen, x - unit, y - unit, unit * 2, unit * 2);
                    else if (Glyph == ChromeGlyph.Restore)
                    { g.DrawLines(pen, new[] { new PointF(x - unit + Dip(2), y - unit), new PointF(x + unit, y - unit), new PointF(x + unit, y + unit - Dip(2)) }); g.DrawRectangle(pen, x - unit, y - unit + Dip(2), unit * 2 - Dip(2), unit * 2 - Dip(2)); }
                    else if (Glyph == ChromeGlyph.Overflow) using (var brush = new SolidBrush(foreground))
                        for (int i = -1; i <= 1; i++) g.FillEllipse(brush, x + i * Dip(5) - Dip(1), y - Dip(1), Dip(2), Dip(2));
                }
            }
            if (Focused && ShowFocusCues) ControlPaint.DrawFocusRectangle(g, Rectangle.Inflate(rect, -Dip(3), -Dip(3)), colors.Primary, BackColor);
        }

        private static GraphicsPath Rounded(Rectangle rect, int radius)
        {
            var path = new GraphicsPath();
            if (radius <= 0) { path.AddRectangle(rect); return path; }
            int d = Math.Min(radius * 2, Math.Min(rect.Width, rect.Height));
            path.AddArc(rect.Left, rect.Top, d, d, 180, 90); path.AddArc(rect.Right - d, rect.Top, d, d, 270, 90);
            path.AddArc(rect.Right - d, rect.Bottom - d, d, d, 0, 90); path.AddArc(rect.Left, rect.Bottom - d, d, d, 90, 90); path.CloseFigure();
            return path;
        }
    }

    internal sealed class HostChrome : Control
    {
        // Shared geometry keeps the WinForms strip and the host layout in sync.
        internal const int HeightDip = 36;
        internal const int CaptionWidthDip = 42;
        internal const int CaptionReserveDip = CaptionWidthDip * 3;
        internal const int TabMinWidthDip = 96;
        internal const int TabMaxWidthDip = 172;
        private const int TabStartDip = 68;
        private const int TabGapDip = 4;
        private const int TabRowInsetDip = 4;
        private const int TabHeightDip = HeightDip - TabRowInsetDip * 2;

        private sealed class Tab
        {
            internal ChromeButton Select, Close;
        }
        private readonly Dictionary<Session, Tab> tabs = new Dictionary<Session, Tab>();
        private readonly List<Session> ordered = new List<Session>();
        private readonly List<Session> visible = new List<Session>();
        private readonly List<Session> overflowed = new List<Session>();
        private readonly ToolTip tips = new ToolTip { InitialDelay = 500, ReshowDelay = 100, AutoPopDelay = 10000, ShowAlways = true };
        private readonly ContextMenuStrip menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = false };
        private readonly ChromeButton overflow;
        private readonly ChromeButton[] captions = new ChromeButton[3];
        private readonly Action<Session> activate, close;
        private readonly Action focusDesktop;
        private readonly Func<int, int> scale;
        private readonly Icon icon;
        private Font chromeFont;
        private Session active;
        private NativePalette palette = new NativePalette(null);

        internal IList<Session> VisibleSessions { get { return visible.AsReadOnly(); } }
        internal IList<Session> OverflowSessions { get { return overflowed.AsReadOnly(); } }
        internal ContextMenuStrip OverflowMenu { get { return menu; } }
        internal ChromeButton SelectButton(Session session) { return tabs[session].Select; }
        internal ChromeButton CloseButton(Session session) { return tabs[session].Close; }

        internal HostChrome(Func<int, int> scale, Icon icon, Action<Session> activate, Action<Session> close, Action<int> caption, Action focusDesktop)
        {
            this.scale = scale; this.icon = icon; this.activate = activate; this.close = close; this.focusDesktop = focusDesktop;
            AccessibleName = "远程桌面连接标签"; AccessibleRole = AccessibleRole.PageTabList; TabStop = false;
            SetStyle(ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.AllPaintingInWmPaint, true);
            overflow = MakeButton("其他 RDP 连接", ChromeGlyph.Overflow);
            overflow.Click += delegate { OpenOverflow(); };
            for (int i = 0; i < 3; i++)
            {
                int index = i;
                captions[i] = MakeButton(i == 0 ? "最小化" : i == 1 ? "最大化" : "关闭窗口", i == 0 ? ChromeGlyph.Minimize : i == 1 ? ChromeGlyph.Maximize : ChromeGlyph.Close);
                captions[i].Caption = true;
                captions[i].Click += delegate { caption(index); };
            }
            menu.Closed += delegate { if (!IsDisposed && IsHandleCreated) BeginInvoke(new Action(focusDesktop)); };
        }

        private ChromeButton MakeButton(string name, ChromeGlyph glyph)
        {
            var button = new ChromeButton { AccessibleName = name, Glyph = glyph, ScaleDip = scale };
            tips.SetToolTip(button, name); Controls.Add(button); return button;
        }

        internal static string Label(Session session)
        {
            const string prefix = "MXterm RDP - ";
            string value = session.Config.title ?? "远程桌面";
            return value.StartsWith(prefix, StringComparison.Ordinal) ? value.Substring(prefix.Length) : value;
        }
        internal static string Status(Session session)
        {
            switch (session.Phase)
            {
                case "login_complete": return "已连接";
                case "connected": return "等待远程登录";
                case "disconnected": return "连接已断开";
                case "fatal": return "连接失败";
                case "reconnecting": return "正在重新连接";
                default: return "正在连接";
            }
        }

        internal void UpdateSessions(IList<Session> sessions, Session selected, Appearance appearance, bool maximized)
        {
            SuspendLayout();
            try
            {
                int fontSize = scale(12);
                if (chromeFont == null || chromeFont.Size != fontSize)
                {
                    Font previous = chromeFont;
                    Font = chromeFont = new Font(SystemFonts.MessageBoxFont.FontFamily, fontSize, GraphicsUnit.Pixel);
                    if (previous != null) previous.Dispose();
                }
                // Close a stale menu before removing any session references it owns.
                if (menu.Visible) menu.Close();
                foreach (Session previous in new List<Session>(tabs.Keys)) if (!sessions.Contains(previous))
                { tips.SetToolTip(tabs[previous].Select, null); tips.SetToolTip(tabs[previous].Close, null); tabs[previous].Select.Dispose(); tabs.Remove(previous); }
                ordered.Clear(); ordered.AddRange(sessions); active = selected;
                palette = new NativePalette(appearance); BackColor = palette.Chrome;
                foreach (Session session in ordered)
                {
                    Tab tab;
                    if (!tabs.TryGetValue(session, out tab))
                    {
                        var select = MakeButton(Label(session), ChromeGlyph.None); select.Session = session; select.AccessibleRole = AccessibleRole.PageTab;
                        var remove = new ChromeButton { Glyph = ChromeGlyph.Close, ScaleDip = scale, AccessibleName = "关闭 " + Label(session) };
                        select.Controls.Add(remove); tips.SetToolTip(remove, remove.AccessibleName);
                        select.Click += delegate { if (ordered.Contains(session)) activate(session); };
                        remove.Click += delegate { if (ordered.Contains(session)) close(session); };
                        select.MouseUp += delegate(object sender, MouseEventArgs e) { if (e.Button == MouseButtons.Middle && ordered.Contains(session)) close(session); };
                        remove.MouseUp += delegate(object sender, MouseEventArgs e) { if (e.Button == MouseButtons.Middle && ordered.Contains(session)) close(session); };
                        tab = new Tab { Select = select, Close = remove }; tabs.Add(session, tab);
                    }
                    tab.Select.Selected = session == active;
                    tab.Select.Palette = tab.Close.Palette = palette;
                    tab.Select.BackColor = palette.Chrome;
                    tab.Close.BackColor = session == active ? palette.Active : palette.Chrome;
                    UpdateStatus(session);
                }
                foreach (ChromeButton button in captions) { button.Palette = palette; button.BackColor = palette.Chrome; button.Invalidate(); }
                captions[1].Glyph = maximized ? ChromeGlyph.Restore : ChromeGlyph.Maximize;
                captions[1].AccessibleName = maximized ? "还原" : "最大化";
                tips.SetToolTip(captions[1], captions[1].AccessibleName);
                overflow.Palette = palette; overflow.BackColor = palette.Chrome;
                menu.BackColor = palette.Panel; menu.ForeColor = palette.Text; menu.Font = Font;
                menu.Renderer = new ToolStripProfessionalRenderer(new MenuColors(palette)) { RoundedEdges = false };
            }
            finally { ResumeLayout(true); }
            // An activation can change only the selected session; no child size
            // or collection mutation then schedules a WinForms layout pass.
            PerformLayout(); Invalidate();
        }

        internal void UpdateStatus(Session session)
        {
            Tab tab;
            if (!tabs.TryGetValue(session, out tab)) return;
            string status = Label(session) + " · " + Status(session);
            tab.Select.AccessibleName = status;
            tab.Select.AccessibleDescription = session == active ? "当前远程桌面" : "切换到此远程桌面";
            tips.SetToolTip(tab.Select, status + "\n" + session.Config.host + ":" + session.Config.port);
            tab.Select.Invalidate();
            foreach (ToolStripItem item in menu.Items) if (item.Tag == session) item.Text = status;
        }

        // Size each tab to its own title instead of a shared fixed width, so a
        // short connection name no longer reserves an empty 200 DIP strip.
        private int NaturalWidth(Session session)
        {
            Size text = TextRenderer.MeasureText(Label(session), Font, new Size(Int32.MaxValue, Int32.MaxValue),
                TextFormatFlags.NoPrefix | TextFormatFlags.SingleLine);
            // Left inset + status dot + gap + label + gap + close button + right inset + tab gap.
            int width = scale(10 + 6 + 7 + 6 + 20 + 4 + TabGapDip) + text.Width;
            return Math.Max(scale(TabMinWidthDip), Math.Min(scale(TabMaxWidthDip), width));
        }

        protected override void OnLayout(LayoutEventArgs e)
        {
            base.OnLayout(e);
            if (overflow == null || captions[2] == null) return;
            int start = scale(TabStartDip), end = Width - scale(CaptionReserveDip);
            int room = Math.Max(1, end - start - scale(20));
            visible.Clear();
            int available = Math.Max(1, room - (ordered.Count > 1 ? scale(28) : 0));
            int used = 0;
            // Fill the strip with as many readable labels as fit; do not squeeze
            // every title down to an ellipsis just to reach the five-tab cap.
            foreach (Session session in ordered)
            {
                if (visible.Count >= 5) break;
                int width = NaturalWidth(session);
                if (visible.Count > 0 && used + width > available) break;
                visible.Add(session); used += width;
            }
            if (active != null && !visible.Contains(active) && visible.Count > 0) visible[visible.Count - 1] = active;
            overflowed.Clear();
            foreach (Session session in ordered) { tabs[session].Select.Visible = visible.Contains(session); if (!visible.Contains(session)) overflowed.Add(session); }
            int budget = Math.Max(1, room - (overflowed.Count > 0 ? scale(28) : 0));
            int[] widths = new int[visible.Count];
            int total = 0;
            for (int i = 0; i < widths.Length; i++) { widths[i] = NaturalWidth(visible[i]); total += widths[i]; }
            // Only compress when the natural widths cannot fit at all; leftover
            // room stays empty rather than stretching short labels back out.
            if (total > budget)
            {
                int each = Math.Max(1, budget / Math.Max(1, widths.Length));
                for (int i = 0; i < widths.Length; i++) widths[i] = Math.Min(widths[i], each);
            }
            int x = start;
            for (int i = 0; i < visible.Count; i++)
            {
                Tab tab = tabs[visible[i]];
                int tabWidth = Math.Max(1, widths[i] - scale(TabGapDip));
                tab.Select.Bounds = new Rectangle(x, scale(TabRowInsetDip), tabWidth, scale(TabHeightDip));
                tab.Close.Bounds = new Rectangle(tabWidth - scale(24), scale(TabRowInsetDip), scale(20), scale(20));
                x += widths[i];
            }
            overflow.Visible = overflowed.Count > 0;
            overflow.Bounds = new Rectangle(x, scale(TabRowInsetDip), scale(24), scale(TabHeightDip));
            tips.SetToolTip(overflow, "其他 RDP 连接（" + overflowed.Count + "）");
            for (int i = 0; i < 3; i++) captions[i].Bounds = new Rectangle(end + scale(CaptionWidthDip * i), 0, scale(CaptionWidthDip), Height);
        }

        internal void OpenOverflow()
        {
            if (overflowed.Count == 0) return;
            menu.Items.Clear();
            foreach (Session session in overflowed)
            {
                var item = new ToolStripMenuItem(Label(session) + " · " + Status(session)) { Tag = session, ToolTipText = session.Config.host + ":" + session.Config.port };
                item.Click += delegate { if (ordered.Contains(session)) activate(session); };
                menu.Items.Add(item);
            }
            menu.MaximumSize = new Size(scale(380), Math.Max(scale(80), Screen.FromControl(this).WorkingArea.Height - Height - scale(24)));
            menu.Show(this, new Point(overflow.Right, Height), ToolStripDropDownDirection.BelowLeft);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            base.OnPaint(e);
            if (icon != null) e.Graphics.DrawIcon(icon, new Rectangle(scale(10), scale(10), scale(16), scale(16)));
            TextRenderer.DrawText(e.Graphics, "RDP", Font, new Rectangle(scale(30), 0, scale(34), Height), palette.Muted, TextFormatFlags.VerticalCenter | TextFormatFlags.NoPrefix);
            using (var pen = new Pen(palette.Line)) e.Graphics.DrawLine(pen, 0, Height - 1, Width, Height - 1);
        }

        protected override void WndProc(ref Message message)
        {
            // Bare chrome passes hit-testing to the top-level Form for native
            // window dragging/resizing. Real child buttons retain normal input.
            if (message.Msg == 0x84) { message.Result = new IntPtr(-1); return; }
            base.WndProc(ref message);
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) { tips.Dispose(); menu.Dispose(); if (chromeFont != null) chromeFont.Dispose(); }
            base.Dispose(disposing);
        }

        private sealed class MenuColors : ProfessionalColorTable
        {
            private readonly NativePalette palette;
            internal MenuColors(NativePalette palette) { this.palette = palette; UseSystemColors = false; }
            public override Color ToolStripDropDownBackground { get { return palette.Panel; } }
            public override Color MenuBorder { get { return palette.Line; } }
            public override Color MenuItemSelected { get { return palette.Hover; } }
            public override Color MenuItemBorder { get { return palette.Line; } }
            public override Color ImageMarginGradientBegin { get { return palette.Panel; } }
            public override Color ImageMarginGradientMiddle { get { return palette.Panel; } }
            public override Color ImageMarginGradientEnd { get { return palette.Panel; } }
        }
    }
}
