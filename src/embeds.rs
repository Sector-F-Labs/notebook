//! Widgets embedded in the editor in place of a paragraph: tables, whose
//! cells hold their inline markdown (ADR-0005), and code blocks with a
//! language picker, a copy button and syntax highlighting (ADR-0006).

use egui::{Id, RichText};
use egui_extras::syntax_highlighting::{self, CodeTheme};

use crate::markdown::{Align, Table};

#[derive(Default)]
pub struct EmbedResponse {
    pub changed: bool,
    /// The user asked to remove the whole table or code block.
    pub delete: bool,
}

/// Languages offered for code blocks; any other fence tag is kept and shown.
const LANGUAGES: &[(&str, &str)] = &[
    ("", "Plain text"),
    ("bash", "Bash"),
    ("c", "C"),
    ("cpp", "C++"),
    ("css", "CSS"),
    ("go", "Go"),
    ("html", "HTML"),
    ("java", "Java"),
    ("js", "JavaScript"),
    ("json", "JSON"),
    ("markdown", "Markdown"),
    ("python", "Python"),
    ("rust", "Rust"),
    ("sql", "SQL"),
    ("toml", "TOML"),
    ("ts", "TypeScript"),
    ("yaml", "YAML"),
];

const CELL_WIDTH: f32 = 140.0;

pub fn table(ui: &mut egui::Ui, table: &mut Table, id: Id) -> EmbedResponse {
    let mut out = EmbedResponse::default();
    let mut command = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::Grid::new(id).spacing([4.0, 4.0]).show(ui, |ui| {
            for r in 0..table.rows.len() {
                for c in 0..table.columns() {
                    let header = r == 0;
                    let align = match table.align[c] {
                        Align::Center => egui::Align::Center,
                        Align::Right => egui::Align::Max,
                        _ => egui::Align::Min,
                    };
                    let mut edit = egui::TextEdit::singleline(&mut table.rows[r][c])
                        .id(id.with((r, c)))
                        .desired_width(CELL_WIDTH)
                        .horizontal_align(align);
                    if header {
                        edit = edit.text_color(ui.visuals().strong_text_color()).hint_text("Heading");
                    }
                    let resp = ui.add_sized([CELL_WIDTH, ui.spacing().interact_size.y], edit);
                    out.changed |= resp.changed();
                    resp.context_menu(|ui| {
                        if let Some(cmd) = table_menu(ui, r, c, table) {
                            command = Some(cmd);
                            ui.close();
                        }
                    });
                }
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button("+ Row").clicked() {
                command = Some(TableCommand::InsertRow(table.rows.len()));
            }
            if ui.small_button("+ Column").clicked() {
                command = Some(TableCommand::InsertColumn(table.columns()));
            }
            ui.label(RichText::new("Right-click a cell for more").weak().small());
        });
    });
    if let Some(cmd) = command {
        if cmd == TableCommand::DeleteTable {
            out.delete = true;
        } else {
            apply(table, cmd);
            out.changed = true;
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TableCommand {
    InsertRow(usize),
    DeleteRow(usize),
    InsertColumn(usize),
    DeleteColumn(usize),
    Align(usize, Align),
    DeleteTable,
}

fn table_menu(ui: &mut egui::Ui, r: usize, c: usize, table: &Table) -> Option<TableCommand> {
    let mut cmd = None;
    if r > 0 && ui.button("Insert Row Above").clicked() {
        cmd = Some(TableCommand::InsertRow(r));
    }
    if ui.button("Insert Row Below").clicked() {
        cmd = Some(TableCommand::InsertRow(r + 1));
    }
    if r > 0 && table.rows.len() > 2 && ui.button("Delete Row").clicked() {
        cmd = Some(TableCommand::DeleteRow(r));
    }
    ui.separator();
    if ui.button("Insert Column Left").clicked() {
        cmd = Some(TableCommand::InsertColumn(c));
    }
    if ui.button("Insert Column Right").clicked() {
        cmd = Some(TableCommand::InsertColumn(c + 1));
    }
    if table.columns() > 1 && ui.button("Delete Column").clicked() {
        cmd = Some(TableCommand::DeleteColumn(c));
    }
    ui.separator();
    ui.menu_button("Align Column", |ui| {
        for (align, label) in
            [(Align::None, "Default"), (Align::Left, "Left"), (Align::Center, "Centre"), (Align::Right, "Right")]
        {
            if ui.radio(table.align[c] == align, label).clicked() {
                cmd = Some(TableCommand::Align(c, align));
            }
        }
    });
    ui.separator();
    if ui.button("Delete Table").clicked() {
        cmd = Some(TableCommand::DeleteTable);
    }
    cmd
}

fn apply(table: &mut Table, cmd: TableCommand) {
    let columns = table.columns();
    match cmd {
        TableCommand::InsertRow(at) => table.rows.insert(at.max(1), vec![String::new(); columns]),
        TableCommand::DeleteRow(at) => {
            table.rows.remove(at);
        }
        TableCommand::InsertColumn(at) => {
            table.align.insert(at, Align::None);
            for row in &mut table.rows {
                row.insert(at, String::new());
            }
        }
        TableCommand::DeleteColumn(at) => {
            table.align.remove(at);
            for row in &mut table.rows {
                row.remove(at);
            }
        }
        TableCommand::Align(c, align) => table.align[c] = align,
        TableCommand::DeleteTable => {}
    }
}

/// The name syntect knows a fence tag's language by: it looks syntaxes up
/// by file extension.
fn syntax_name(lang: &str) -> String {
    match lang.to_lowercase().as_str() {
        "" => "txt",
        "rust" => "rs",
        "python" => "py",
        "javascript" => "js",
        "typescript" => "ts",
        "bash" | "shell" | "zsh" => "sh",
        "markdown" => "md",
        "yml" => "yaml",
        "c++" => "cpp",
        "golang" => "go",
        other => return other.to_string(),
    }
    .to_string()
}

pub fn code(ui: &mut egui::Ui, lang: &mut String, code: &mut String, id: Id) -> EmbedResponse {
    let mut out = EmbedResponse::default();
    let fill = ui.visuals().code_bg_color;
    egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
        ui.horizontal(|ui| {
            let shown = LANGUAGES.iter().find(|(tag, _)| tag == lang).map_or(lang.as_str(), |(_, name)| name);
            egui::ComboBox::from_id_salt(id.with("lang")).selected_text(shown).show_ui(ui, |ui| {
                for (tag, name) in LANGUAGES {
                    if ui.selectable_label(lang == tag, *name).clicked() && lang != tag {
                        *lang = tag.to_string();
                        out.changed = true;
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Delete").on_hover_text("Delete this code block").clicked() {
                    out.delete = true;
                }
                if ui.small_button("Copy").on_hover_text("Copy the code").clicked() {
                    ui.ctx().copy_text(code.clone());
                }
            });
        });
        let theme = CodeTheme::from_memory(ui.ctx(), ui.style());
        let language = syntax_name(lang);
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = syntax_highlighting::highlight(ui.ctx(), ui.style(), &theme, text.as_str(), &language);
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let resp = ui.add(
            egui::TextEdit::multiline(code)
                .id(id.with("text"))
                .code_editor()
                .frame(egui::Frame::NONE)
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter),
        );
        out.changed |= resp.changed();
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_commands_keep_rows_square() {
        let mut t = Table::new(2, 1);
        apply(&mut t, TableCommand::InsertColumn(1));
        apply(&mut t, TableCommand::InsertRow(1));
        assert_eq!(t.columns(), 3);
        assert!(t.rows.iter().all(|r| r.len() == 3));
        assert_eq!(t.rows.len(), 3);
        apply(&mut t, TableCommand::DeleteColumn(0));
        apply(&mut t, TableCommand::DeleteRow(2));
        assert_eq!((t.columns(), t.rows.len()), (2, 2));
        // The header row stays first.
        apply(&mut t, TableCommand::InsertRow(0));
        assert_eq!(t.rows.len(), 3);
    }
}
