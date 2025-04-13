using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Data;
using System.Drawing;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace MyDictionary
{
    public class CenteredMessageBox : Form
    {
        public CenteredMessageBox(string message, string title)
        {
            this.Text = title;
            this.Size = new Size(300, 200);
            this.StartPosition = FormStartPosition.CenterParent;
            this.FormBorderStyle = FormBorderStyle.FixedDialog;
            this.MaximizeBox = false;
            this.MinimizeBox = false;

            // レイアウト全体
            var layout = new TableLayoutPanel
            {
                Dock = DockStyle.Fill,
                RowCount = 3,
                ColumnCount = 1,
                Padding = new Padding(5),
            };
            layout.RowStyles.Add(new RowStyle(SizeType.Percent, 60)); // メッセージ部
            layout.RowStyles.Add(new RowStyle(SizeType.Percent, 20)); // 空白
            layout.RowStyles.Add(new RowStyle(SizeType.Percent, 30)); // ボタン部

            var lbl = new Label
            {
                Text = message,
                TextAlign = ContentAlignment.MiddleCenter,
                Dock = DockStyle.Fill,
                Font = new Font("メイリオ", 10),
            };

            var btnOk = new Button
            {
                Text = "OK",
                Anchor = AnchorStyles.None,
                AutoSize = true,
            };
            btnOk.Click += (s, e) => this.Close();

            layout.Controls.Add(lbl, 0, 0);
            layout.Controls.Add(btnOk, 0, 2);

            this.Controls.Add(layout);
        }

        private void InitializeComponent()
        {

        }
    }
}
