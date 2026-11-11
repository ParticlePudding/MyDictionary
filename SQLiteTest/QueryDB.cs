using System;
using System.Collections.Generic;
using System.Data;
using System.Data.SQLite;

class QueryDB
{
    private static readonly TimeZoneInfo JstZone = GetJstZone();

    private static TimeZoneInfo GetJstZone()
    {
        foreach (var id in new[] { "Tokyo Standard Time", "Asia/Tokyo" })
        {
            try { return TimeZoneInfo.FindSystemTimeZoneById(id); }
            catch (TimeZoneNotFoundException) { }
            catch (InvalidTimeZoneException) { }
        }
        return TimeZoneInfo.Local;
    }

    /// <summary>
    /// UTC(DB保存値)を日本時間に変換する
    /// </summary>
    public static DateTime ToJst(object utcValue)
    {
        DateTime utc = DateTime.SpecifyKind(Convert.ToDateTime(utcValue), DateTimeKind.Utc);
        return TimeZoneInfo.ConvertTimeFromUtc(utc, JstZone);
    }

    /// <summary>
    /// dictionaryテーブルを作成する
    /// </summary>
    public static void CreateTable(SQLiteConnection connection)
    {
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
    /// 検索語句をGLOBパターンに変換する。
    /// 空なら全件、ワイルドカード(* ? [)を含まなければ部分一致にする。
    /// </summary>
    private static string ToGlobPattern(string input)
    {
        if (string.IsNullOrWhiteSpace(input)) return "*";
        input = input.Trim();
        return input.IndexOfAny(new[] { '*', '?', '[' }) >= 0 ? input : $"*{input}*";
    }

    /// <summary>
    /// 検索結果のDataTableを返す
    /// </summary>
    public static DataTable DisplayData(SQLiteConnection connection, string searchWord, string searchMeaning, string searchTag)
    {
        string addSearchTag = "";
        bool hasTag = !string.IsNullOrWhiteSpace(searchTag);
        if (hasTag)
        {
            addSearchTag = "AND (tag1 = @searchTag OR tag2 = @searchTag OR tag3 = @searchTag)";
        }

        string selectQuery = $@"SELECT * FROM dictionary WHERE word GLOB @searchWord AND meaning GLOB @searchMeaning {addSearchTag} ORDER BY id;";

        var table = new DataTable("Table");
        using (var command = new SQLiteCommand(selectQuery, connection))
        {
            command.Parameters.AddWithValue("@searchWord", ToGlobPattern(searchWord));
            command.Parameters.AddWithValue("@searchMeaning", ToGlobPattern(searchMeaning));
            if (hasTag)
            {
                command.Parameters.AddWithValue("@searchTag", searchTag.Trim());
            }
            using (var adapter = new SQLiteDataAdapter(command))
            {
                adapter.Fill(table);
            }
        }

        // 表示用の列を追加(LastUpdateは文字列列として日本時間で保持)
        if (!table.Columns.Contains("Tags"))
        {
            table.Columns.Add("Tags", typeof(string));
        }
        var lastUpdateCol = table.Columns["lastupdate"]!;
        lastUpdateCol.ColumnName = "LastUpdate";
        var display = table.Clone();
        display.Columns["LastUpdate"]!.DataType = typeof(string);

        foreach (DataRow row in table.Rows)
        {
            var tags = new List<string>();
            foreach (var name in new[] { "tag1", "tag2", "tag3" })
            {
                var t = row[name] as string;
                if (!string.IsNullOrEmpty(t)) tags.Add(t);
            }
            var newRow = display.NewRow();
            foreach (DataColumn col in table.Columns)
            {
                if (col.ColumnName == "LastUpdate") continue;
                newRow[col.ColumnName] = row[col];
            }
            newRow["Tags"] = string.Join(", ", tags);
            newRow["LastUpdate"] = row["LastUpdate"] == DBNull.Value
                ? ""
                : ToJst(row["LastUpdate"]).ToString("yyyy/MM/dd HH:mm:ss");
            display.Rows.Add(newRow);
        }
        return display;
    }

    /// <summary>
    /// 最終更新日時(JST)を取得する。レコードがなければnull
    /// </summary>
    public static DateTime? GetLastUpdate(SQLiteConnection connection)
    {
        using (var command = new SQLiteCommand("SELECT MAX(lastupdate) FROM dictionary", connection))
        {
            var result = command.ExecuteScalar();
            if (result == null || result == DBNull.Value) return null;
            return ToJst(result);
        }
    }

    /// <summary>
    /// 新しいレコードを登録する
    /// </summary>
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
    /// 指定したIDのレコードを削除する
    /// </summary>
    public static void DeleteRecord(SQLiteConnection connection, int id)
    {
        using (var command = new SQLiteCommand("DELETE FROM dictionary WHERE id = @id;", connection))
        {
            command.Parameters.AddWithValue("@id", id);
            command.ExecuteNonQuery();
        }
    }
}
