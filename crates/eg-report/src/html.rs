//! HTML 渲染（`SPEC-report.md` §3.5：单文件、内联样式、无外部引用、可打印）。
//!
//! 所有动态文本都做 HTML 转义；不使用脚本、外部 CSS、字体或图片。

use crate::table::{ColumnKind, Table};

/// 渲染单文件 HTML（页面标题 + 8 张表 + 页脚说明）。
pub(crate) fn render(tables: &[Table], title: &str, footer: &str) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>");
    out.push_str(&escape(title));
    out.push_str("</title>\n<style>\n");
    out.push_str(
        "body{font-family:-apple-system,\"PingFang SC\",\"Microsoft YaHei\",sans-serif;\
         margin:24px;color:#1f2328;background:#fff;}\
         h1{font-size:20px;margin:0 0 4px 0;}\
         h2{font-size:16px;margin:28px 0 8px 0;border-left:4px solid #2f6feb;padding-left:8px;}\
         table{border-collapse:collapse;width:100%;font-size:12px;}\
         th,td{border:1px solid #d0d7de;padding:4px 6px;text-align:left;vertical-align:top;}\
         th{background:#f6f8fa;font-weight:600;}\
         td.num{text-align:right;font-variant-numeric:tabular-nums;}\
         tfoot{color:#57606a;font-size:11px;margin-top:16px;display:block;}\
         p.note{color:#57606a;font-size:11px;margin-top:16px;}\
         @media print{body{margin:8mm;} h2{page-break-after:avoid;} table{font-size:10px;}}\n",
    );
    out.push_str("</style>\n</head>\n<body>\n<h1>");
    out.push_str(&escape(title));
    out.push_str("</h1>\n");
    for (section_index, table) in tables.iter().enumerate() {
        out.push_str("<h2 id=\"sheet-");
        out.push_str(&(section_index.saturating_add(1)).to_string());
        out.push_str("\">");
        out.push_str(&escape(table.sheet.name()));
        out.push_str("</h2>\n<table>\n<thead><tr>");
        for column in &table.columns {
            out.push_str("<th>");
            out.push_str(&escape(&column.title));
            out.push_str("</th>");
        }
        out.push_str("</tr></thead>\n<tbody>\n");
        for row in &table.rows {
            out.push_str("<tr>");
            for (index, cell) in row.iter().enumerate() {
                let kind = table
                    .columns
                    .get(index)
                    .map_or(ColumnKind::Text, |column| column.kind);
                let class = match kind {
                    ColumnKind::Text => "text",
                    ColumnKind::Score | ColumnKind::Percent | ColumnKind::Int => "num",
                };
                out.push_str("<td class=\"");
                out.push_str(class);
                out.push_str("\">");
                out.push_str(&escape(&cell.display()));
                out.push_str("</td>");
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody>\n</table>\n");
    }
    out.push_str("<p class=\"note\">");
    out.push_str(&escape(footer));
    out.push_str("</p>\n</body>\n</html>\n");
    out.into_bytes()
}

/// HTML 转义（文本与属性值共用）。
#[must_use]
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 供测试断言：HTML 中是否含外部引用（`http://`/`https://`/`src=`/`@import`）。
#[cfg(test)]
#[must_use]
pub(crate) fn has_external_reference(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("src=")
        || lower.contains("@import")
        || lower.contains("<script")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Cell, Column, Sheet};

    fn sample() -> Table {
        Table {
            sheet: Sheet::Students,
            columns: vec![Column {
                title: "<姓名>".to_owned(),
                kind: ColumnKind::Text,
            }],
            rows: vec![vec![Cell::Text("张三 & 李四".to_owned())]],
        }
    }

    #[test]
    fn escapes_and_has_no_external_references() {
        let html = String::from_utf8(render(&[sample()], "期中 <数学>", "页脚")).unwrap();
        assert!(html.contains("&lt;姓名&gt;"));
        assert!(html.contains("张三 &amp; 李四"));
        assert!(html.contains("期中 &lt;数学&gt;"));
        assert!(!has_external_reference(&html));
        assert!(html.contains("<h2 id=\"sheet-1\">成绩单</h2>"));
    }
}
