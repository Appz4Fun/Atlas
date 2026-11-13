//! Pure terminal presentation retained independently of catalog storage.

use crate::ui::{self, Column, Line, Table, fmt_size};

/// The category it's known to be in. The terminal says what's known, and only the API reports the default number.
pub fn category_cell(cats: &std::collections::HashMap<i64, i64>, id: i64) -> String {
    cats.get(&id).and_then(|c| crate::category::label(*c)).unwrap_or_else(|| "-".into())
}

pub fn password_text(password: Option<crate::archive::Password>) -> Option<(&'static str, &'static str)> {
    use crate::archive::Password;
    match password {
        Some(Password::Yes) => Some((" - passworded", "red")),
        Some(Password::Maybe) => Some((" - maybe passworded", "yellow")),
        Some(Password::No) | None => None,
    }
}

/// What the release holds, as far as its archives say. `Line::plain`, soo a hostile name isnt read as markup.
pub fn inner_table(inner: &crate::archive::store::InnerView, width: usize) -> Option<Table> {
    if inner.files.is_empty() {
        return None;
    }
    let mut table = Table::new(vec![Column::new("name").style("white").flex(), Column::new("size").style("dim")])
        .title(format!("inner files ({})", inner.files.len()))
        .no_header();
    table.gap = 1;
    for file in inner.files.iter().take(30) {
        table.add_row(vec![Line::plain(ui::ellipsize(&file.name, width)), Line::plain(fmt_size(file.size))]);
    }
    Some(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Password;
    use crate::archive::store::{InnerFileRow, InnerView};

    fn row(i: i64, name: &str) -> InnerFileRow {
        InnerFileRow { seq: i, name: name.into(), size: Some(10), source: 2 }
    }

    #[test]
    fn category_cell_shows_a_label_or_a_dash() {
        let cats = std::collections::HashMap::from([(1, 5040)]);
        assert_eq!(category_cell(&cats, 1), "TV/HD");
        assert_eq!(category_cell(&cats, 2), "-");
    }

    #[test]
    fn inner_table_lists_rows_and_marks_password() {
        assert!(inner_table(&InnerView::default(), 80).is_none());

        let files: Vec<InnerFileRow> = (0..40).map(|i| row(i, &format!("file{i}.mkv"))).collect();
        let table = inner_table(&InnerView { password: None, files, ..InnerView::default() }, 80).unwrap();
        assert_eq!(table.rows.len(), 30);
        assert_eq!(table.title.as_deref(), Some("inner files (40)"));

        let hostile =
            inner_table(&InnerView { password: None, files: vec![row(0, "[red]x.mkv")], ..InnerView::default() }, 80)
                .unwrap();
        assert_eq!(hostile.rows[0][0].text(), "[red]x.mkv");

        assert_eq!(password_text(Some(Password::Yes)), Some((" - passworded", "red")));
        assert_eq!(password_text(Some(Password::Maybe)), Some((" - maybe passworded", "yellow")));
        assert_eq!(password_text(Some(Password::No)), None);
        assert_eq!(password_text(None), None);
    }
}
