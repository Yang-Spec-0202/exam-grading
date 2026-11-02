//! XLSX 渲染（`rust_xlsxwriter 0.102`；`SPEC-report.md` §3.5 工作表顺序与列定义）。
//!
//! 数字列保持数值类型（分数 0.001 精度、得分率 0–1 并用百分比显示格式），
//! 便于教师二次计算；文本列不做任何藏写或截断。

use crate::error::{ReportError, Result};
use crate::table::{Cell, ColumnKind, Table};
use rust_xlsxwriter::{Format, FormatBorder, Workbook};

/// 把全部工作表渲染为一个 XLSX 字节缓冲。
///
/// # Errors
///
/// 工作表名、行列数或格式非法时返回 [`ReportError::Xlsx`]。
pub(crate) fn render(tables: &[Table], title: &str) -> Result<Vec<u8>> {
    let mut workbook = Workbook::new();
    let properties = rust_xlsxwriter::DocProperties::new()
        .set_title(title)
        .set_author("local-teacher")
        .set_company("exam-grading");
    workbook.set_properties(&properties);

    let header_format = Format::new()
        .set_bold()
        .set_background_color(0xF2_F2_F2)
        .set_border(FormatBorder::Thin);
    let text_format = Format::new().set_border(FormatBorder::Thin);
    let score_format = Format::new()
        .set_num_format("0.###")
        .set_border(FormatBorder::Thin);
    let percent_format = Format::new()
        .set_num_format("0.0%")
        .set_border(FormatBorder::Thin);
    let int_format = Format::new()
        .set_num_format("0")
        .set_border(FormatBorder::Thin);

    for table in tables {
        let worksheet = workbook.add_worksheet();
        worksheet
            .set_name(table.sheet.name())
            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
        for (index, column) in table.columns.iter().enumerate() {
            let col = u16::try_from(index)
                .map_err(|_| ReportError::Xlsx("列数超出 XLSX 上限".to_owned()))?;
            worksheet
                .write_string_with_format(0, col, &column.title, &header_format)
                .map_err(|error| ReportError::Xlsx(error.to_string()))?;
            let width: f64 = match column.kind {
                ColumnKind::Text => {
                    if table.sheet == crate::table::Sheet::Meta && index == 1 {
                        90.0
                    } else {
                        18.0
                    }
                }
                ColumnKind::Score | ColumnKind::Percent | ColumnKind::Int => 10.0,
            };
            worksheet
                .set_column_width(col, width)
                .map_err(|error| ReportError::Xlsx(error.to_string()))?;
        }
        for (row_index, row) in table.rows.iter().enumerate() {
            let excel_row = u32::try_from(row_index.saturating_add(1))
                .map_err(|_| ReportError::Xlsx("行数超出 XLSX 上限".to_owned()))?;
            for (column_index, cell) in row.iter().enumerate() {
                let col = u16::try_from(column_index)
                    .map_err(|_| ReportError::Xlsx("列数超出 XLSX 上限".to_owned()))?;
                match cell {
                    Cell::Text(value) => {
                        worksheet
                            .write_string_with_format(excel_row, col, value, &text_format)
                            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
                    }
                    Cell::Score(milli) => {
                        worksheet
                            .write_number_with_format(
                                excel_row,
                                col,
                                milli_to_points(*milli),
                                &score_format,
                            )
                            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
                    }
                    Cell::Percent(ppm) => {
                        worksheet
                            .write_number_with_format(
                                excel_row,
                                col,
                                ppm_to_ratio(*ppm),
                                &percent_format,
                            )
                            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
                    }
                    Cell::Int(value) => {
                        worksheet
                            .write_number_with_format(excel_row, col, *value as f64, &int_format)
                            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
                    }
                    Cell::Blank => {}
                }
            }
        }
        if !table.rows.is_empty() {
            let last_row = u32::try_from(table.rows.len())
                .map_err(|_| ReportError::Xlsx("行数超出 XLSX 上限".to_owned()))?;
            let last_col = u16::try_from(table.columns.len().saturating_sub(1))
                .map_err(|_| ReportError::Xlsx("列数超出 XLSX 上限".to_owned()))?;
            worksheet
                .autofilter(0, 0, last_row, last_col)
                .map_err(|error| ReportError::Xlsx(error.to_string()))?;
        }
        worksheet
            .set_freeze_panes(1, 0)
            .map_err(|error| ReportError::Xlsx(error.to_string()))?;
    }
    workbook
        .save_to_buffer()
        .map_err(|error| ReportError::Xlsx(error.to_string()))
}

/// 千分 → 分（浮点仅用于 XLSX 数值单元格，不参与汇总）。
fn milli_to_points(milli: i64) -> f64 {
    // 千分以内精度在 f64 下可精确表示（|milli| 远小于 2^53）。
    milli as f64 / 1000.0
}

/// ppm → 0–1 比率（浮点仅用于 XLSX 百分比格式）。
fn ppm_to_ratio(ppm: u32) -> f64 {
    f64::from(ppm) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Column, Sheet};

    #[test]
    fn renders_workbook_with_sheet_names() {
        let table = Table {
            sheet: Sheet::Meta,
            columns: vec![
                Column {
                    title: "项目".to_owned(),
                    kind: ColumnKind::Text,
                },
                Column {
                    title: "值".to_owned(),
                    kind: ColumnKind::Text,
                },
            ],
            rows: vec![vec![
                Cell::Text("试卷".to_owned()),
                Cell::Text("期中".to_owned()),
            ]],
        };
        let bytes = render(&[table], "测试").unwrap();
        assert_eq!(&bytes[..2], b"PK");
        assert!(bytes.len() > 1000);
    }
}
