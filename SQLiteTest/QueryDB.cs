using Microsoft.VisualBasic;
using System;
using System.CodeDom;
using System.ComponentModel.Design;
using System.Data;
using System.Data.SQLite;
using System.Windows.Forms;
using System.Windows.Input;

class QueryDB
{
    /// <summary>
    /// データベースにdictionaryテーブルを作成するメソッド
    /// </summary>
    public static void CreateTable(SQLiteConnection connection)
    {
        // テーブル作成
        string queryCreateTable = @"CREATE TABLE IF NOT EXISTS dictionary (
                                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                                    word TEXT NOT NULL,
                                    meaning TEXT NOT NULL,
                                    tag1 TEXT,
                                    tag2 TEXT,
                                    tag3 TEXT,
                                    lastupdate DATETIME DEFAULT CURRENT_TIMESTAMP
                                    );";
        using (var command = new SQLiteCommand(queryCreateTable, connection))
        {
            command.ExecuteNonQuery();
        }
    }

    /// <summary>
    /// データベースから検索結果のテーブルを取得するメソッド
    /// </summary>
    public static DataTable DisplayData(SQLiteConnection connection, string searchWord, string searchMeaning, string searchTag, DataTable table)
    {
        // 検索条件が空の場合は全件取得
        if (string.IsNullOrEmpty(searchWord))
        {
            searchWord = "*";
        }
        if (string.IsNullOrEmpty(searchMeaning))
        {
            searchMeaning = "*";
        }
        string addSearchTag = "";
        if (!string.IsNullOrEmpty(searchTag))
        {
            addSearchTag ="AND (tag1 = @searchTag OR tag2 = @searchTag OR tag3 = @searchTag)";
        }

        string selectQuery = $@"SELECT * FROM dictionary WHERE word GLOB @searchWord AND meaning GLOB @searchMeaning {addSearchTag};";

        using (var command = new SQLiteCommand(selectQuery, connection))
        {
            command.Parameters.AddWithValue("@searchWord", searchWord);
            command.Parameters.AddWithValue("@searchMeaning", searchMeaning);
            if (!string.IsNullOrEmpty(searchTag))
            {
                command.Parameters.AddWithValue("@searchTag", searchTag);
            }
            using (SQLiteDataAdapter adapter = new SQLiteDataAdapter(command))
            {
                adapter.Fill(table);

                // データテーブルを整形
                table.Columns.Add("Tags", typeof(string));
                foreach (DataRow row in table.Rows)
                {
                   
                    List<string> tags = new List<string>();

                    // NULL値ではないタグを集める
                    var tag1 = row["tag1"] as string;
                    if (!string.IsNullOrEmpty(tag1))
                    {
                        tags.Add(tag1);
                    }
                    var tag2 = row["tag2"] as string;
                    if (!string.IsNullOrEmpty(tag2))
                    {
                        tags.Add(tag2);
                    }
                    var tag3 = row["tag3"] as string;
                    if (!string.IsNullOrEmpty(tag3))
                    {
                        tags.Add(tag3);
                    }

                    // タグをカンマ区切りの文字列として結合
                    row["Tags"] = string.Join(", ", tags);

                    // 更新時刻を日本時刻に変換
                    if (row["LastUpdate"] != DBNull.Value)
                    {
                        DateTime utcTime = Convert.ToDateTime(row["LastUpdate"]);
                        TimeZoneInfo jstZone = TimeZoneInfo.FindSystemTimeZoneById("Asia/Tokyo");
                        DateTime jstTime = TimeZoneInfo.ConvertTimeFromUtc(utcTime, jstZone);
                        row["LastUpdate"] = jstTime;
                    }                
                }
            }
        }

        return table;
    }

    /// <summary>
    /// 新しいレコードを登録するメソッド
    /// </summary>
    /// <param name="connection"> SQLiteConnection </param>
    /// <param name="word"> 登録する単語 </param>
    /// <param name="meaning"> 単語の意味 </param>
    /// <param name="tag"> 単語のタグ（種別） </param>
    public static void InsertRecord(SQLiteConnection connection, string word, string meaning,
        string tag1, string tag2, string tag3)
    {
        string queryInsert = @"INSERT INTO dictionary (word, meaning, tag1, tag2, tag3, lastupdate)
                               VALUES (@word, @meaning, @tag1, @tag2, @tag3, CURRENT_TIMESTAMP)";
        using (var command = new SQLiteCommand(queryInsert, connection))
        {
            command.Parameters.AddWithValue("@word", word);
            command.Parameters.AddWithValue("@meaning", meaning);
            command.Parameters.AddWithValue("@tag1", tag1);
            command.Parameters.AddWithValue("@tag2", tag2);
            command.Parameters.AddWithValue("@tag3", tag3);
            command.ExecuteNonQuery();
        }

    }

    /// <summary>
    /// 指定したIDのレコードを削除するメソッド
    /// </summary>
    /// <param name="connection"> SQLiteConnection </param>
    /// <param name="id"> 削除するレコードのID </param>
    public static void DeleteRecord(SQLiteConnection connection, int id)
    {
        string queryDelete = @"DELETE FROM dictionary WHERE id = @id;";
        using (var command = new SQLiteCommand(queryDelete, connection))
        {
            command.Parameters.AddWithValue("@id", id);
            command.ExecuteNonQuery();
        }
    }
}





