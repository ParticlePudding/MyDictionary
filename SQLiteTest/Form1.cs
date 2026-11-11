using Microsoft.VisualBasic;
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Data;
using System.Data.SQLite;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace MyDictionary
{

    public partial class Form1 : Form
    {
        private SQLiteConnection? connection;
        private int selectedRecordId = -1;

        public Form1()
        {
            InitializeComponent();
        }

        private void UpdateLastUpdateLabel()
        {
            if (connection == null) return;
            var last = QueryDB.GetLastUpdate(connection);
            toolStripStatusLabel1.Text = last.HasValue
                ? $"最終更新日時: {last.Value:yyyy/MM/dd HH:mm:ss}"
                : "最終更新日時: なし";
        }

        private void ShowCenteredMessage(string message, string title = "")
        {
            using (var msg = new CenteredMessageBox(message, title))
            {
                msg.ShowDialog(this);
            }
        }

        private void SetDataGridView_Layout(DataGridView dataGridView)
        {
            // 列の幅を指定する
            dataGridView.Columns["Meaning"].Width = 300;
            dataGridView.Columns["LastUpdate"].Width = 160;
            dataGridView.Columns["LastUpdate"].AutoSizeMode = DataGridViewAutoSizeColumnMode.None;
            // セルのテキスト折り返しを有効にする
            dataGridView.DefaultCellStyle.WrapMode = DataGridViewTriState.True;
            // 行の高さを自動調整する
            dataGridView.AutoSizeRowsMode = DataGridViewAutoSizeRowsMode.AllCells;
            // 列を非表示にする
            dataGridView.Columns["ID"].Visible = false;
            dataGridView.Columns["Tag1"].Visible = false;
            dataGridView.Columns["Tag2"].Visible = false;
            dataGridView.Columns["Tag3"].Visible = false;
        }

        private void Form1_Load(object sender, EventArgs e)
        {   
            // 設定・DBは %AppData%\MyDictionary に保存する(単一ファイル配布のため)
            try
            {
                string dir = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "MyDictionary");
                Directory.CreateDirectory(dir);
                string dbPath = Path.Combine(dir, "dictionary.db");
                connection = new SQLiteConnection($"Data Source={dbPath};Version=3;");
                connection.Open();
                QueryDB.CreateTable(connection);
                UpdateLastUpdateLabel();
            }
            catch (Exception ex)
            {
                connection?.Dispose();
                connection = null;
                MessageBox.Show($"データベースに接続できません。\n{ex.Message}");
            }

            // データグリッドビューの初期化
            dataGridView1.AutoGenerateColumns = true;
            dataGridView1.AutoSizeRowsMode = DataGridViewAutoSizeRowsMode.AllCells;
            dataGridView1.DefaultCellStyle.WrapMode = DataGridViewTriState.True;

            DataTable table_startup = new DataTable("Table");

            table_startup.Columns.Add("ID", typeof(int));
            table_startup.Columns.Add("Word");
            table_startup.Columns.Add("Meaning");
            table_startup.Columns.Add("Tags");
            table_startup.Columns.Add("Tag1");
            table_startup.Columns.Add("Tag2");
            table_startup.Columns.Add("Tag3");
            table_startup.Columns.Add("LastUpdate");

            dataGridView1.DataSource = table_startup;

            SetDataGridView_Layout(dataGridView1);

        }

        private void Form1_Close(object sender, FormClosingEventArgs e)
        {
            // フォームが閉じられるときの処理
            // データベース接続を閉じる
            if (connection != null)
            {
                connection.Close();
                connection.Dispose();
            }

        }

        public static void ShowMessageCentered(Form owner, string message, string title)
        {
            using (Form dummy = new Form())
            {
                dummy.StartPosition = FormStartPosition.Manual;

                // 親フォームの中央に配置
                dummy.Size = new Size(1, 1);
                dummy.Location = new Point(
                    owner.Location.X + owner.Width / 2,
                    owner.Location.Y + owner.Height / 2);

                dummy.ShowInTaskbar = false;
                dummy.Opacity = 0; // 完全に透明にする
                dummy.Show();
                dummy.BringToFront();

                MessageBox.Show(dummy, message, title);

                dummy.Close();
            }
        }



        /// <summary>
        /// 検索語句を使用してDBを検索し、検索結果を表示する
        /// </summary>
        /// <param name="sender"></param>
        /// <param name="e"></param>
        private void btnSearch_Click(object sender, EventArgs e)
        {
            if (connection == null)
            {
                ShowCenteredMessage("データベースに接続されていません。", "エラー");
                return;
            }

            //if (string.IsNullOrEmpty(txtSearchWord.Text))
            //{
            //    ShowCenteredMessage("検索語句を入力してください。", "エラー");
            //    return;
            //}

            string inputSearchWord = txtSearchWord.Text;
            string inputSearchMeaning = txtSearchMeaning.Text;
            string inputSearchTag = txtSearchTag.Text;

            DataTable tableResult = QueryDB.DisplayData(connection, inputSearchWord, inputSearchMeaning, inputSearchTag);
            dataGridView1.DataSource = tableResult;

            SetDataGridView_Layout(dataGridView1);
            dataGridView1.PerformLayout();
        }

        /// <summary>
        /// Enterキー入力したとき、検索ボタンを押したときの処理を実行する
        /// </summary>
        private void txtSearchWord_KeyDown(object sender, KeyEventArgs e)
        {
            if (e.KeyCode == Keys.Enter)
            {
                e.SuppressKeyPress = true; // エンターキーの音を消す
                btnSearch_Click(sender, e); // 検索ボタンのクリックイベントを呼び出す
            }
        }

        /// <summary>
        /// Enterキー入力したとき、検索ボタンを押したときの処理を実行する
        /// </summary>
        private void txtSearchMeaning_KeyDown(object sender, KeyEventArgs e) => txtSearchWord_KeyDown(sender, e);

        /// <summary>
        /// Enterキー入力したとき、検索ボタンを押したときの処理を実行する
        /// </summary>
        private void txtSearchTag_KeyDown(object sender, KeyEventArgs e) => txtSearchWord_KeyDown(sender, e);

        /// <summary>
        /// DBを新しく作成する
        /// </summary>
        private void btnCreateDB_Click(object sender, EventArgs e)
        {
            if (connection == null)
            {
                ShowCenteredMessage("データベースに接続されていません。", "エラー");
                return;
            }
            QueryDB.CreateTable(connection);
        }


        /// <summary>
        /// 語句、意味、その他情報を入力し、DBに登録する
        /// </summary>
        private void btnRegister_Click(object sender, EventArgs e)
        {
            if (connection == null)
            {
                ShowCenteredMessage("データベースに接続されていません。", "エラー");
                return;
            }

            string inputWord = txtWord.Text.Trim();
            string inputMeaning = txtMeaning.Text.Trim();
            string inputTag1 = txtTag1.Text.Trim();
            string inputTag2 = txtTag2.Text.Trim();
            string inputTag3 = txtTag3.Text.Trim();

            // 入力チェック
            if (string.IsNullOrWhiteSpace(inputWord) || string.IsNullOrWhiteSpace(inputMeaning))
            {
                string errorMessage = "";
                if (string.IsNullOrWhiteSpace(inputWord) && string.IsNullOrWhiteSpace(inputMeaning))
                {
                    errorMessage = "語句と意味を入力してください。";
                }
                else if (string.IsNullOrWhiteSpace(inputWord))
                {
                    errorMessage = "語句を入力してください。";
                }
                else if (string.IsNullOrWhiteSpace(inputMeaning))
                {
                    errorMessage = "意味を入力してください。";
                }
                ShowCenteredMessage(errorMessage, "");
                return;
            }

            QueryDB.InsertRecord(connection, inputWord, inputMeaning, inputTag1, inputTag2, inputTag3);

            txtWord.Clear();
            txtMeaning.Clear();
            txtTag1.Clear();
            txtTag2.Clear();
            txtTag3.Clear();
            UpdateLastUpdateLabel();
            ShowCenteredMessage("登録が完了しました。", "");
        }

        /// <summary>
        /// データグリッドビュー上で右クリックしたとき
        /// 選択されているセルを右クリックメニューで選択する
        /// </summary>
        private void DataGridView1_MouseDown(object sender, MouseEventArgs e)
        {
            if (e.Button == MouseButtons.Right)
            {
                var hit = dataGridView1.HitTest(e.X, e.Y);
                if (hit.RowIndex >= 0 && hit.ColumnIndex >= 0)
                {
                    if (!dataGridView1.Rows[hit.RowIndex].Cells[hit.ColumnIndex].Selected)
                    {
                        dataGridView1.CurrentCell = dataGridView1[hit.ColumnIndex, hit.RowIndex];
                        dataGridView1.Rows[hit.RowIndex].Cells[hit.ColumnIndex].Selected = true;
                    }
                }
            }

        }

        /// <summary>
        /// 右クリックメニューのCopyをクリックしたとき
        /// 選択セルの内容をクリップボードにコピーする
        /// </summary>
        private void CopyItem_Click(object sender, EventArgs e)
        {
            if (dataGridView1.SelectedCells.Count > 0)
            {
                var selectedText = new StringBuilder();
                int currentRow = -1;

                foreach (DataGridViewCell cell in dataGridView1.SelectedCells)
                {
                    if (cell.RowIndex != currentRow)
                    {
                        if (currentRow != -1)
                        {
                            selectedText.AppendLine();
                        }
                        currentRow = cell.RowIndex;
                    }
                    else
                    {
                        selectedText.Append("\t");
                    }
                    selectedText.Append(cell.Value?.ToString());
                }
                Clipboard.SetText(selectedText.ToString());
            }
        }

        /// <summary>
        /// 右クリックメニューのDelete Recordをクリックしたとき
        /// 選択セルの内容をDBから削除する
        /// </summary>
        private void DeleteRecord_Click(object sender, EventArgs e)
        {
            if (connection == null)
            {
                ShowCenteredMessage("データベースに接続されていません。", "エラー");
                return;
            }
            // 選択されているセルが属する行(重複なし)だけを対象にする
            var rows = dataGridView1.SelectedCells
                .Cast<DataGridViewCell>()
                .Select(c => c.RowIndex)
                .Where(i => i >= 0 && !dataGridView1.Rows[i].IsNewRow)
                .Distinct()
                .OrderByDescending(i => i)
                .ToList();
            if (rows.Count == 0) return;

            var confirm = MessageBox.Show(this, $"{rows.Count} 件のレコードを削除します。よろしいですか?", "削除の確認",
                MessageBoxButtons.YesNo, MessageBoxIcon.Warning, MessageBoxDefaultButton.Button2);
            if (confirm != DialogResult.Yes) return;

            using (var tx = connection.BeginTransaction())
            {
                foreach (int i in rows)
                {
                    selectedRecordId = Convert.ToInt32(dataGridView1.Rows[i].Cells["ID"].Value);
                    QueryDB.DeleteRecord(connection, selectedRecordId);
                }
                tx.Commit();
            }
            foreach (int i in rows)
            {
                dataGridView1.Rows.RemoveAt(i);
            }
            UpdateLastUpdateLabel();
        }
    }
}