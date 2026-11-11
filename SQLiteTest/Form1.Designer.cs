using System;
using System.Drawing;
using System.Text;
using System.Windows.Forms;

namespace MyDictionary
{
    partial class Form1
    {
        /// <summary>
        /// Required designer variable.
        /// </summary>
        private System.ComponentModel.IContainer components = null;



        /// <summary>
        /// Clean up any resources being used.
        /// </summary>
        /// <param name="disposing">true if managed resources should be disposed; otherwise, false.</param>
        protected override void Dispose(bool disposing)
        {
            if (disposing && (components != null))
            {
                components.Dispose();
            }
            base.Dispose(disposing);
        }

        #region Windows Form Designer generated code

        /// <summary>
        /// Required method for Designer support - do not modify
        /// the contents of this method with the code editor.
        /// </summary>
        private void InitializeComponent()
        {
            components = new System.ComponentModel.Container();
            DataGridViewCellStyle dataGridViewCellStyle1 = new DataGridViewCellStyle();
            DataGridViewCellStyle dataGridViewCellStyle2 = new DataGridViewCellStyle();
            DataGridViewCellStyle dataGridViewCellStyle3 = new DataGridViewCellStyle();
            contextMenu_GridView = new ContextMenuStrip(components);
            copyItem2 = new ToolStripMenuItem();
            deleteRecord = new ToolStripMenuItem();
            statusStrip1 = new StatusStrip();
            toolStripStatusLabel1 = new ToolStripStatusLabel();
            tabPageSetting = new TabPage();
            lblCreateDB = new Label();
            btnCreateDB = new Button();
            tabPageRegiseter = new TabPage();
            txtTag3 = new TextBox();
            lblRegTag3 = new Label();
            txtTag2 = new TextBox();
            lblRegTag2 = new Label();
            txtTag1 = new TextBox();
            txtMeaning = new TextBox();
            txtWord = new TextBox();
            btnRegist = new Button();
            lblRegMeaning = new Label();
            lblRegWord = new Label();
            lblRegTag1 = new Label();
            lblRegisterMsg = new Label();
            tabPageSearch = new TabPage();
            txtSearchTag = new TextBox();
            lblSearchTag = new Label();
            txtSearchMeaning = new TextBox();
            lblSearchMeaning = new Label();
            dataGridView1 = new DataGridView();
            txtSearchWord = new TextBox();
            btnSearch = new Button();
            lblSerachWord = new Label();
            tabControlRead = new TabControl();
            contextMenu_GridView.SuspendLayout();
            statusStrip1.SuspendLayout();
            tabPageSetting.SuspendLayout();
            tabPageRegiseter.SuspendLayout();
            tabPageSearch.SuspendLayout();
            ((System.ComponentModel.ISupportInitialize)dataGridView1).BeginInit();
            tabControlRead.SuspendLayout();
            SuspendLayout();
            // 
            // contextMenu_GridView
            // 
            contextMenu_GridView.ImageScalingSize = new Size(24, 24);
            contextMenu_GridView.Items.AddRange(new ToolStripItem[] { copyItem2, deleteRecord });
            contextMenu_GridView.Name = "contextMenu2";
            contextMenu_GridView.Size = new Size(189, 68);
            // 
            // copyItem2
            // 
            copyItem2.Name = "copyItem2";
            copyItem2.Size = new Size(188, 32);
            copyItem2.Text = "コピー";
            copyItem2.Click += CopyItem_Click;
            // 
            // deleteRecord
            // 
            deleteRecord.Name = "deleteRecord";
            deleteRecord.Size = new Size(188, 32);
            deleteRecord.Text = "選択行を削除";
            deleteRecord.Click += DeleteRecord_Click;
            // 
            // statusStrip1
            // 
            statusStrip1.ImageScalingSize = new Size(24, 24);
            statusStrip1.Items.AddRange(new ToolStripItem[] { toolStripStatusLabel1 });
            statusStrip1.Location = new Point(0, 520);
            statusStrip1.Name = "statusStrip1";
            statusStrip1.Padding = new Padding(2, 0, 14, 0);
            statusStrip1.Size = new Size(818, 32);
            statusStrip1.TabIndex = 6;
            statusStrip1.Text = "statusStrip1";
            // 
            // toolStripStatusLabel1
            // 
            toolStripStatusLabel1.Name = "toolStripStatusLabel1";
            toolStripStatusLabel1.Size = new Size(181, 25);
            toolStripStatusLabel1.Text = "toolStripStatusLabel1";
            // 
            // tabPageSetting
            // 
            tabPageSetting.Controls.Add(lblCreateDB);
            tabPageSetting.Controls.Add(btnCreateDB);
            tabPageSetting.Location = new Point(4, 34);
            tabPageSetting.Name = "tabPageSetting";
            tabPageSetting.Padding = new Padding(3);
            tabPageSetting.Size = new Size(810, 482);
            tabPageSetting.TabIndex = 2;
            tabPageSetting.Text = "設定";
            tabPageSetting.UseVisualStyleBackColor = true;
            // 
            // lblCreateDB
            // 
            lblCreateDB.AutoSize = true;
            lblCreateDB.Location = new Point(24, 78);
            lblCreateDB.Name = "lblCreateDB";
            lblCreateDB.Size = new Size(172, 25);
            lblCreateDB.TabIndex = 1;
            lblCreateDB.Text = "データベースを作成する";
            // 
            // btnCreateDB
            // 
            btnCreateDB.Location = new Point(212, 74);
            btnCreateDB.Name = "btnCreateDB";
            btnCreateDB.Size = new Size(112, 34);
            btnCreateDB.TabIndex = 0;
            btnCreateDB.Text = "作成";
            btnCreateDB.UseVisualStyleBackColor = true;
            btnCreateDB.Click += btnCreateDB_Click;
            // 
            // tabPageRegiseter
            // 
            tabPageRegiseter.Controls.Add(txtTag3);
            tabPageRegiseter.Controls.Add(lblRegTag3);
            tabPageRegiseter.Controls.Add(txtTag2);
            tabPageRegiseter.Controls.Add(lblRegTag2);
            tabPageRegiseter.Controls.Add(txtTag1);
            tabPageRegiseter.Controls.Add(txtMeaning);
            tabPageRegiseter.Controls.Add(txtWord);
            tabPageRegiseter.Controls.Add(btnRegist);
            tabPageRegiseter.Controls.Add(lblRegMeaning);
            tabPageRegiseter.Controls.Add(lblRegWord);
            tabPageRegiseter.Controls.Add(lblRegTag1);
            tabPageRegiseter.Controls.Add(lblRegisterMsg);
            tabPageRegiseter.Location = new Point(4, 34);
            tabPageRegiseter.Name = "tabPageRegiseter";
            tabPageRegiseter.Padding = new Padding(3);
            tabPageRegiseter.Size = new Size(810, 482);
            tabPageRegiseter.TabIndex = 1;
            tabPageRegiseter.Text = "登録";
            tabPageRegiseter.UseVisualStyleBackColor = true;
            // 
            // txtTag3
            // 
            txtTag3.Location = new Point(112, 366);
            txtTag3.Name = "txtTag3";
            txtTag3.Size = new Size(150, 31);
            txtTag3.TabIndex = 9;
            // 
            // lblRegTag3
            // 
            lblRegTag3.AutoSize = true;
            lblRegTag3.Location = new Point(24, 369);
            lblRegTag3.Name = "lblRegTag3";
            lblRegTag3.Size = new Size(50, 25);
            lblRegTag3.TabIndex = 12;
            lblRegTag3.Text = "タグ3";
            // 
            // txtTag2
            // 
            txtTag2.Location = new Point(112, 314);
            txtTag2.Name = "txtTag2";
            txtTag2.Size = new Size(150, 31);
            txtTag2.TabIndex = 8;
            // 
            // lblRegTag2
            // 
            lblRegTag2.AutoSize = true;
            lblRegTag2.Location = new Point(24, 317);
            lblRegTag2.Name = "lblRegTag2";
            lblRegTag2.Size = new Size(50, 25);
            lblRegTag2.TabIndex = 10;
            lblRegTag2.Text = "タグ2";
            // 
            // txtTag1
            // 
            txtTag1.Location = new Point(112, 262);
            txtTag1.Name = "txtTag1";
            txtTag1.Size = new Size(150, 31);
            txtTag1.TabIndex = 7;
            // 
            // txtMeaning
            // 
            txtMeaning.Location = new Point(112, 143);
            txtMeaning.Multiline = true;
            txtMeaning.Name = "txtMeaning";
            txtMeaning.Size = new Size(576, 98);
            txtMeaning.TabIndex = 6;
            // 
            // txtWord
            // 
            txtWord.Location = new Point(112, 91);
            txtWord.Name = "txtWord";
            txtWord.Size = new Size(576, 31);
            txtWord.TabIndex = 5;
            // 
            // btnRegist
            // 
            btnRegist.Location = new Point(150, 424);
            btnRegist.Name = "btnRegist";
            btnRegist.Size = new Size(112, 34);
            btnRegist.TabIndex = 10;
            btnRegist.Text = "登録";
            btnRegist.UseVisualStyleBackColor = true;
            btnRegist.Click += btnRegister_Click;
            // 
            // lblRegMeaning
            // 
            lblRegMeaning.AutoSize = true;
            lblRegMeaning.Location = new Point(24, 146);
            lblRegMeaning.Name = "lblRegMeaning";
            lblRegMeaning.Size = new Size(48, 25);
            lblRegMeaning.TabIndex = 4;
            lblRegMeaning.Text = "意味";
            // 
            // lblRegWord
            // 
            lblRegWord.AutoSize = true;
            lblRegWord.Location = new Point(24, 94);
            lblRegWord.Name = "lblRegWord";
            lblRegWord.Size = new Size(48, 25);
            lblRegWord.TabIndex = 3;
            lblRegWord.Text = "単語";
            // 
            // lblRegTag1
            // 
            lblRegTag1.AutoSize = true;
            lblRegTag1.Location = new Point(24, 265);
            lblRegTag1.Name = "lblRegTag1";
            lblRegTag1.Size = new Size(50, 25);
            lblRegTag1.TabIndex = 2;
            lblRegTag1.Text = "タグ1";
            // 
            // lblRegisterMsg
            // 
            lblRegisterMsg.AutoSize = true;
            lblRegisterMsg.Location = new Point(20, 22);
            lblRegisterMsg.Name = "lblRegisterMsg";
            lblRegisterMsg.Size = new Size(212, 25);
            lblRegisterMsg.TabIndex = 1;
            lblRegisterMsg.Text = "登録語句を入力してください";
            // 
            // tabPageSearch
            // 
            tabPageSearch.Controls.Add(txtSearchTag);
            tabPageSearch.Controls.Add(lblSearchTag);
            tabPageSearch.Controls.Add(txtSearchMeaning);
            tabPageSearch.Controls.Add(lblSearchMeaning);
            tabPageSearch.Controls.Add(dataGridView1);
            tabPageSearch.Controls.Add(txtSearchWord);
            tabPageSearch.Controls.Add(btnSearch);
            tabPageSearch.Controls.Add(lblSerachWord);
            tabPageSearch.Location = new Point(4, 34);
            tabPageSearch.Name = "tabPageSearch";
            tabPageSearch.Padding = new Padding(3);
            tabPageSearch.Size = new Size(810, 482);
            tabPageSearch.TabIndex = 0;
            tabPageSearch.Text = "検索";
            tabPageSearch.UseVisualStyleBackColor = true;
            // 
            // txtSearchTag
            // 
            txtSearchTag.Location = new Point(629, 18);
            txtSearchTag.Name = "txtSearchTag";
            txtSearchTag.Size = new Size(150, 31);
            txtSearchTag.TabIndex = 5;
            txtSearchTag.KeyDown += txtSearchTag_KeyDown;
            // 
            // lblSearchTag
            // 
            lblSearchTag.AutoSize = true;
            lblSearchTag.Location = new Point(584, 21);
            lblSearchTag.Name = "lblSearchTag";
            lblSearchTag.Size = new Size(39, 25);
            lblSearchTag.TabIndex = 7;
            lblSearchTag.Text = "Tag";
            // 
            // txtSearchMeaning
            // 
            txtSearchMeaning.Location = new Point(368, 18);
            txtSearchMeaning.Name = "txtSearchMeaning";
            txtSearchMeaning.Size = new Size(150, 31);
            txtSearchMeaning.TabIndex = 4;
            txtSearchMeaning.KeyDown += txtSearchMeaning_KeyDown;
            // 
            // lblSearchMeaning
            // 
            lblSearchMeaning.AutoSize = true;
            lblSearchMeaning.Location = new Point(281, 21);
            lblSearchMeaning.Name = "lblSearchMeaning";
            lblSearchMeaning.Size = new Size(81, 25);
            lblSearchMeaning.TabIndex = 5;
            lblSearchMeaning.Text = "Meaning";
            // 
            // dataGridView1
            // 
            dataGridView1.AllowUserToAddRows = false;
            dataGridView1.AllowUserToDeleteRows = false;
            dataGridView1.Anchor = AnchorStyles.Top | AnchorStyles.Bottom | AnchorStyles.Left | AnchorStyles.Right;
            dataGridView1.AutoSizeColumnsMode = DataGridViewAutoSizeColumnsMode.Fill;
            dataGridView1.AutoSizeRowsMode = DataGridViewAutoSizeRowsMode.AllCells;
            dataGridViewCellStyle1.Alignment = DataGridViewContentAlignment.MiddleLeft;
            dataGridViewCellStyle1.BackColor = SystemColors.Control;
            dataGridViewCellStyle1.Font = new Font("Yu Gothic UI", 9F);
            dataGridViewCellStyle1.ForeColor = SystemColors.WindowText;
            dataGridViewCellStyle1.SelectionBackColor = SystemColors.Highlight;
            dataGridViewCellStyle1.SelectionForeColor = SystemColors.HighlightText;
            dataGridViewCellStyle1.WrapMode = DataGridViewTriState.True;
            dataGridView1.ColumnHeadersDefaultCellStyle = dataGridViewCellStyle1;
            dataGridView1.ColumnHeadersHeightSizeMode = DataGridViewColumnHeadersHeightSizeMode.AutoSize;
            dataGridView1.ContextMenuStrip = contextMenu_GridView;
            dataGridViewCellStyle2.Alignment = DataGridViewContentAlignment.MiddleLeft;
            dataGridViewCellStyle2.BackColor = SystemColors.Window;
            dataGridViewCellStyle2.Font = new Font("Yu Gothic UI", 9F);
            dataGridViewCellStyle2.ForeColor = SystemColors.ControlText;
            dataGridViewCellStyle2.SelectionBackColor = SystemColors.Highlight;
            dataGridViewCellStyle2.SelectionForeColor = SystemColors.HighlightText;
            dataGridViewCellStyle2.WrapMode = DataGridViewTriState.True;
            dataGridView1.DefaultCellStyle = dataGridViewCellStyle2;
            dataGridView1.Location = new Point(8, 113);
            dataGridView1.Name = "dataGridView1";
            dataGridView1.ReadOnly = true;
            dataGridViewCellStyle3.Alignment = DataGridViewContentAlignment.MiddleLeft;
            dataGridViewCellStyle3.BackColor = SystemColors.Control;
            dataGridViewCellStyle3.Font = new Font("Yu Gothic UI", 9F);
            dataGridViewCellStyle3.ForeColor = SystemColors.WindowText;
            dataGridViewCellStyle3.SelectionBackColor = SystemColors.Highlight;
            dataGridViewCellStyle3.SelectionForeColor = SystemColors.HighlightText;
            dataGridViewCellStyle3.WrapMode = DataGridViewTriState.True;
            dataGridView1.RowHeadersDefaultCellStyle = dataGridViewCellStyle3;
            dataGridView1.RowHeadersVisible = false;
            dataGridView1.RowHeadersWidth = 62;
            dataGridView1.SelectionMode = DataGridViewSelectionMode.CellSelect;
            dataGridView1.Size = new Size(794, 362);
            dataGridView1.TabIndex = 7;
            dataGridView1.MouseDown += DataGridView1_MouseDown;
            // 
            // txtSearchWord
            // 
            txtSearchWord.Location = new Point(83, 18);
            txtSearchWord.Name = "txtSearchWord";
            txtSearchWord.Size = new Size(150, 31);
            txtSearchWord.TabIndex = 3;
            txtSearchWord.KeyDown += txtSearchWord_KeyDown;
            // 
            // btnSearch
            // 
            btnSearch.Location = new Point(663, 64);
            btnSearch.Name = "btnSearch";
            btnSearch.Size = new Size(112, 34);
            btnSearch.TabIndex = 6;
            btnSearch.Text = "検索";
            btnSearch.UseVisualStyleBackColor = true;
            btnSearch.Click += btnSearch_Click;
            // 
            // lblSerachWord
            // 
            lblSerachWord.AutoSize = true;
            lblSerachWord.Location = new Point(21, 21);
            lblSerachWord.Name = "lblSerachWord";
            lblSerachWord.Size = new Size(56, 25);
            lblSerachWord.TabIndex = 0;
            lblSerachWord.Text = "Word";
            // 
            // tabControlRead
            // 
            tabControlRead.Controls.Add(tabPageSearch);
            tabControlRead.Controls.Add(tabPageRegiseter);
            tabControlRead.Controls.Add(tabPageSetting);
            tabControlRead.Dock = DockStyle.Fill;
            tabControlRead.Font = new Font("Yu Gothic UI", 9F);
            tabControlRead.Location = new Point(0, 0);
            tabControlRead.Name = "tabControlRead";
            tabControlRead.SelectedIndex = 0;
            tabControlRead.Size = new Size(818, 520);
            tabControlRead.TabIndex = 4;
            // 
            // Form1
            // 
            AutoScaleDimensions = new SizeF(144F, 144F);
            AutoScaleMode = AutoScaleMode.Dpi;
            ClientSize = new Size(818, 552);
            Controls.Add(tabControlRead);
            Controls.Add(statusStrip1);
            Name = "Form1";
            ShowIcon = false;
            Text = "MyDictionary";
            FormClosing += Form1_Close;
            Load += Form1_Load;
            contextMenu_GridView.ResumeLayout(false);
            statusStrip1.ResumeLayout(false);
            statusStrip1.PerformLayout();
            tabPageSetting.ResumeLayout(false);
            tabPageSetting.PerformLayout();
            tabPageRegiseter.ResumeLayout(false);
            tabPageRegiseter.PerformLayout();
            tabPageSearch.ResumeLayout(false);
            tabPageSearch.PerformLayout();
            ((System.ComponentModel.ISupportInitialize)dataGridView1).EndInit();
            tabControlRead.ResumeLayout(false);
            ResumeLayout(false);
            PerformLayout();
        }

        #endregion
        private ContextMenuStrip contextMenu_GridView;
        private ToolStripMenuItem copyItem2;
        private ToolStripMenuItem deleteRecord;
        private StatusStrip statusStrip1;
        private ToolStripStatusLabel toolStripStatusLabel1;
        private TabPage tabPageSetting;
        private Label lblCreateDB;
        private Button btnCreateDB;
        private TabPage tabPageRegiseter;
        private TextBox txtMeaning;
        private TextBox txtWord;
        private Button btnRegist;
        private Label lblRegMeaning;
        private Label lblRegWord;
        private Label lblRegTag1;
        private Label lblRegisterMsg;
        private TabPage tabPageSearch;
        private DataGridView dataGridView1;
        private TextBox txtSearchWord;
        private Button btnSearch;
        private Label lblSerachWord;
        private TabControl tabControlRead;
        private TextBox txtTag1;
        private TextBox txtTag3;
        private Label lblRegTag3;
        private TextBox txtTag2;
        private Label lblRegTag2;
        private TextBox txtSearchTag;
        private Label lblSearchTag;
        private TextBox txtSearchMeaning;
        private Label lblSearchMeaning;
    }
}   