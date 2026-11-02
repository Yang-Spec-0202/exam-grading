//! CSV 渲染（`SPEC-report.md` §3.5：UTF-8 BOM、与 XLSX 同列并追加整数列）。
//!
//! - 首 3 字节为 UTF-8 BOM（Excel 直开中文不乱码）。
//! - 行尾 CRLF，字段按 RFC 4180 加引号。
//! - 每个工作表一个文件；分数追加 `_milli` 整数列，得分率追加 `_ppm` 整数列，保证无损复算。

#[cfg(test)]
use crate::rounding::{display_percent, display_score};
use crate::table::{Cell, ColumnKind, Table};

/// UTF-8 BOM。
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// 渲染单表 CSV。
pub(crate) fn render(table: &Table) -> Vec<u8> {
    let mut out = String::new();
    let mut headers: Vec<String> = table
        .columns
        .iter()
        .map(|column| column.title.clone())
        .collect();
    for column in &table.columns {
        match column.kind {
            ColumnKind::Score => headers.push(format!("{}_milli", column.title)),
            ColumnKind::Percent => headers.push(format!("{}_ppm", column.title)),
            ColumnKind::Text | ColumnKind::Int => {}
        }
    }
    push_row(&mut out, &headers);
    for row in &table.rows {
        let mut values: Vec<String> = row.iter().map(Cell::display).collect();
        for (index, column) in table.columns.iter().enumerate() {
            let cell = row.get(index);
            match (column.kind, cell) {
                (ColumnKind::Score, Some(Cell::Score(milli))) => values.push(milli.to_string()),
                (ColumnKind::Percent, Some(Cell::Percent(ppm))) => values.push(ppm.to_string()),
                (ColumnKind::Score | ColumnKind::Percent, _) => values.push(String::new()),
                _ => {}
            }
        }
        push_row(&mut out, &values);
    }
    let mut bytes = Vec::with_capacity(out.len().saturating_add(BOM.len()));
    bytes.extend_from_slice(BOM);
    bytes.extend_from_slice(out.as_bytes());
    bytes
}

/// 单行写入（CRLF 结尾）。
fn push_row(out: &mut String, values: &[String]) {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&escape(value));
    }
    out.push_str("\r\n");
}

/// RFC 4180 字段转义。
fn escape(value: &str) -> String {
    let needs_quote = value
        .chars()
        .any(|ch| matches!(ch, ',' | '"' | '\n' | '\r'));
    if !needs_quote {
        return value.to_owned();
    }
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('"');
    for ch in value.chars() {
        if ch == '"' {
            quoted.push('"');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// 单元格文本（仅测试使用）。
#[cfg(test)]
#[must_use]
pub(crate) fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Score(milli) => display_score(*milli),
        Cell::Percent(ppm) => display_percent(*ppm),
        other => other.display(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Column, Sheet};

    fn sample() -> Table {
        Table {
            sheet: Sheet::Students,
            columns: vec![
                Column {
                    title: "姓名".to_owned(),
                    kind: ColumnKind::Text,
                },
                Column {
                    title: "得分".to_owned(),
                    kind: ColumnKind::Score,
                },
                Column {
                    title: "得分率".to_owned(),
                    kind: ColumnKind::Percent,
                },
            ],
            rows: vec![
                vec![
                    Cell::Text("张三, 初一".to_owned()),
                    Cell::Score(2500),
                    Cell::Percent(833_333),
                ],
                vec![Cell::Text("李四".to_owned()), Cell::Blank, Cell::Blank],
            ],
        }
    }

    #[test]
    fn starts_with_bom_and_crlf() {
        let bytes = render(&sample());
        assert_eq!(&bytes[..3], BOM);
        let text = String::from_utf8_lossy(&bytes[3..]);
        assert!(text.contains("姓名,得分,得分率,得分_milli,得分率_ppm\r\n"));
        assert!(text.contains("\"张三, 初一\",2.5,83.3%,2500,833333\r\n"));
        assert!(text.contains("李四,—,—,,\r\n"));
    }

    #[test]
    fn escapes_quotes() {
        assert_eq!(escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape("plain"), "plain");
        assert_eq!(cell_text(&Cell::Score(2500)), "2.5");
    }
}
