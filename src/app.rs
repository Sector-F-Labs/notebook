//! The window: Patina's frame with the toolbar in its header bar, the notes
//! sidebar, and the editor (or the markdown source) for the open note.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use egui::{Key, KeyboardShortcut, Modifiers, RichText};
use patina::frame::{self, TitleBar};
use patina::skin::{self, Skin};

use crate::config::Config;
use crate::editor::{BlockStyle, Editor};
use crate::markdown::Inline;
use crate::notes::{self, Entry};

/// How long after the last edit a note is saved.
const AUTOSAVE: f64 = 1.5;

struct OpenNote {
    path: PathBuf,
    /// The editor's edit count when last saved or loaded.
    saved_edits: u64,
    /// When the last unsaved edit happened.
    edited_at: f64,
}

/// Something asked for from the header bar, done once it's drawn.
enum Action {
    PickFolder,
    NewNote,
    NewFolder,
    RenameNote,
    Save,
    TrashNote,
    Quit,
    Undo,
    Redo,
    SelectAll,
    SetSkin(String),
    ReloadSkins,
    CopySkins,
    OpenSkinsFolder,
    SystemTitleBar(bool),
    ToggleSource,
    InsertTable,
    Block(BlockStyle),
    Inline(Inline),
}

enum Dialog {
    NewNote { dir: PathBuf, name: String },
    NewFolder { dir: PathBuf, name: String },
    Rename { path: PathBuf, name: String },
    Delete { path: PathBuf },
}

pub struct App {
    config: Config,
    skins: Vec<Skin>,
    skin_errors: Vec<String>,
    folder: Option<PathBuf>,
    entries: Vec<Entry>,
    note: Option<OpenNote>,
    editor: Editor,
    /// The markdown source while the Markdown view is on.
    source: Option<String>,
    dialog: Option<Dialog>,
    dialog_error: Option<String>,
    /// The latest error, shown at the bottom of the window until dismissed.
    error: Option<String>,
    /// A passing message, shown like `error` but not in red.
    notice: Option<String>,
    picked: Option<mpsc::Receiver<PathBuf>>,
    /// Screenshot mode: where to save, and whether one was asked for.
    screenshot: Option<(PathBuf, bool)>,
    forced_skin: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        patina::appearance::watch(&cc.egui_ctx);
        let config = Config::load();
        let (skins, skin_errors) = load_skins();
        let mut app = Self {
            config,
            skins,
            skin_errors,
            folder: None,
            entries: Vec::new(),
            note: None,
            editor: Editor::new(egui::Id::new("note-editor")),
            source: None,
            dialog: None,
            dialog_error: None,
            error: None,
            notice: None,
            picked: None,
            screenshot: std::env::var_os("NOTEBOOK_SCREENSHOT").map(|p| (PathBuf::from(p), false)),
            forced_skin: None,
        };
        // For screenshots: a skin for this run only, not saved.
        app.forced_skin = std::env::var("NOTEBOOK_SKIN").ok();
        app.apply_skin(&cc.egui_ctx);
        match std::env::var("NOTEBOOK_THEME").as_deref() {
            Ok("light") => cc.egui_ctx.set_theme(egui::ThemePreference::Light),
            Ok("dark") => cc.egui_ctx.set_theme(egui::ThemePreference::Dark),
            _ => {}
        }
        match open {
            Some(path) if path.is_dir() => app.open_folder(path),
            Some(path) => {
                // A note inside the remembered workspace opens there;
                // anything else opens on its own.
                if let Some(folder) = app.config.folder.clone().filter(|f| path.starts_with(f)) {
                    app.open_folder(folder);
                }
                app.open_note(path);
            }
            None => {
                if let Some(folder) = app.config.folder.clone().filter(|f| f.is_dir()) {
                    app.open_folder(folder);
                }
            }
        }
        app
    }

    fn skin(&self) -> &Skin {
        let name = self.forced_skin.as_deref().or(self.config.skin.as_deref()).unwrap_or(HOME_SKIN);
        self.skins.iter().find(|s| s.name == name).unwrap_or(&self.skins[0])
    }

    fn apply_skin(&self, ctx: &egui::Context) {
        self.skin().apply(ctx);
    }

    // -----------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------

    fn open_folder(&mut self, folder: PathBuf) {
        self.save();
        self.entries = notes::scan(&folder);
        let first = notes::first_note(&self.entries);
        self.config.folder = Some(folder.clone());
        self.config.save();
        self.folder = Some(folder);
        match first {
            Some(path) => self.open_note(path),
            None => self.close_note(),
        }
    }

    fn refresh(&mut self) {
        if let Some(folder) = &self.folder {
            self.entries = notes::scan(folder);
        }
    }

    fn open_note(&mut self, path: PathBuf) {
        if self.note.as_ref().is_some_and(|n| n.path == path) {
            return;
        }
        self.save();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.editor.load(&text);
                self.source = self.source.is_some().then_some(text);
                self.note = Some(OpenNote { path, saved_edits: self.editor.edits(), edited_at: 0.0 });
            }
            Err(e) => self.error = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn close_note(&mut self) {
        self.save();
        self.note = None;
        self.editor.load("");
        self.source = None;
    }

    fn dirty(&self) -> bool {
        self.note.as_ref().is_some_and(|n| n.saved_edits != self.editor.edits())
    }

    /// Writes the open note if it changed since it was loaded or saved.
    fn save(&mut self) {
        if !self.dirty() {
            return;
        }
        let Some(note) = &mut self.note else { return };
        let text = match &self.source {
            Some(source) => source.clone(),
            None => self.editor.markdown(),
        };
        match notes::write_atomic(&note.path, &text) {
            Ok(()) => note.saved_edits = self.editor.edits(),
            Err(e) => self.error = Some(format!("Could not save {}: {e}", note.path.display())),
        }
    }

    fn create_note(&mut self, dir: &Path, name: &str) -> Result<(), String> {
        notes::valid_name(name)?;
        let path = dir.join(notes::note_file_name(name));
        if path.exists() {
            return Err(format!("{} already exists", notes::display_name(&path)));
        }
        let title = notes::display_name(&path);
        notes::write_atomic(&path, &format!("# {title}\n")).map_err(|e| e.to_string())?;
        self.refresh();
        self.open_note(path);
        self.editor.cursor_to_first_line_end();
        Ok(())
    }

    fn create_folder(&mut self, dir: &Path, name: &str) -> Result<(), String> {
        notes::valid_name(name)?;
        std::fs::create_dir(dir.join(name.trim())).map_err(|e| e.to_string())?;
        self.refresh();
        Ok(())
    }

    /// Renames or moves a note or folder, following the open note.
    fn move_entry(&mut self, from: &Path, to: PathBuf) -> Result<(), String> {
        if to.exists() {
            return Err(format!("{} already exists", to.display()));
        }
        if to.starts_with(from) {
            return Err("A folder can't go inside itself".into());
        }
        self.save();
        std::fs::rename(from, &to).map_err(|e| e.to_string())?;
        if let Some(note) = &mut self.note {
            note.path = notes::moved_path(&note.path, from, &to);
        }
        self.refresh();
        Ok(())
    }

    fn rename(&mut self, path: &Path, name: &str) -> Result<(), String> {
        notes::valid_name(name)?;
        let file = if path.is_dir() { name.trim().to_string() } else { notes::note_file_name(name) };
        let to = path.with_file_name(file);
        if to == path {
            return Ok(());
        }
        self.move_entry(path, to)
    }

    fn delete(&mut self, path: &Path) -> Result<(), String> {
        if self.note.as_ref().is_some_and(|n| n.path.starts_with(path)) {
            self.close_note();
        }
        trash::delete(path).map_err(|e| e.to_string())?;
        self.refresh();
        Ok(())
    }

    fn pick_folder(&mut self) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new().set_title("Open Folder").pick_folder() {
                let _ = tx.send(path);
            }
        });
        self.picked = Some(rx);
    }

    fn toggle_source(&mut self) {
        match self.source.take() {
            Some(source) => {
                let dirty = self.dirty();
                self.editor.load(&source);
                if let Some(note) = &mut self.note {
                    // Loading resets the count; keep unsaved changes unsaved.
                    note.saved_edits = if dirty { u64::MAX } else { self.editor.edits() };
                }
            }
            None => self.source = Some(self.editor.markdown()),
        }
    }

    // -----------------------------------------------------------------------
    // Shortcuts
    // -----------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let pressed = |mods, key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(mods, key)));
        if pressed(cmd, Key::O) {
            self.pick_folder();
        }
        if pressed(cmd_shift, Key::N) {
            self.request_new_folder(None);
        }
        if pressed(cmd, Key::N) {
            self.request_new_note(None);
        }
        if pressed(cmd, Key::S) {
            self.save();
        }
        if pressed(cmd, Key::Q) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if pressed(cmd_shift, Key::M) && self.note.is_some() {
            self.toggle_source();
        }
        if pressed(Modifiers::NONE, Key::F2)
            && let Some(note) = &self.note
        {
            self.dialog = Some(Dialog::Rename { name: notes::display_name(&note.path), path: note.path.clone() });
        }

        // Formatting, only while the note text has the keyboard.
        if self.source.is_some() || !ctx.memory(|m| m.has_focus(egui::Id::new("note-editor"))) {
            return;
        }
        let now = ctx.input(|i| i.time);
        let inline = [
            (cmd, Key::B, Inline::Bold),
            (cmd, Key::I, Inline::Italic),
            (cmd, Key::E, Inline::Code),
            (cmd_shift, Key::X, Inline::Strike),
        ];
        for (mods, key, style) in inline {
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(mods, key))) {
                self.editor.toggle_inline(style, now);
            }
        }
        let blocks = [
            (cmd, Key::Num0, BlockStyle::Paragraph),
            (cmd, Key::Num1, BlockStyle::Heading(1)),
            (cmd, Key::Num2, BlockStyle::Heading(2)),
            (cmd, Key::Num3, BlockStyle::Heading(3)),
            (cmd_shift, Key::Num8, BlockStyle::Bullet),
            (cmd_shift, Key::Num7, BlockStyle::Ordered),
            (cmd_shift, Key::L, BlockStyle::Task),
        ];
        for (mods, key, style) in blocks {
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(mods, key))) {
                self.editor.set_block(style, now);
            }
        }
    }

    fn request_new_note(&mut self, dir: Option<PathBuf>) {
        if let Some(dir) = dir.or_else(|| self.target_dir()) {
            self.dialog = Some(Dialog::NewNote { dir, name: String::new() });
            self.dialog_error = None;
        }
    }

    fn request_new_folder(&mut self, dir: Option<PathBuf>) {
        if let Some(dir) = dir.or_else(|| self.target_dir()) {
            self.dialog = Some(Dialog::NewFolder { dir, name: String::new() });
            self.dialog_error = None;
        }
    }

    /// Where a new note or folder goes: the open note's folder, else the
    /// workspace.
    fn target_dir(&self) -> Option<PathBuf> {
        let folder = self.folder.as_ref()?;
        Some(
            self.note
                .as_ref()
                .and_then(|n| n.path.parent())
                .filter(|p| p.starts_with(folder))
                .map_or_else(|| folder.clone(), Path::to_path_buf),
        )
    }

    // -----------------------------------------------------------------------
    // UI
    // -----------------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui, style: &frame::FrameStyle) {
        let custom = !self.config.system_title_bar;
        let title = match &self.note {
            Some(note) => format!("{}{}", notes::display_name(&note.path), if self.dirty() { " •" } else { "" }),
            None => "Notebook".into(),
        };
        let editing = self.note.is_some() && self.source.is_none();
        let mut left = Vec::new();
        let mut right = Vec::new();
        bar_controls(ui, style);
        TitleBar::new(&title, style).custom(custom).menu_bar(true).show(
            ui,
            |ui| self.menus(ui, &mut left),
            |ui| {
                if self.note.is_none() {
                    return;
                }
                if ui
                    .selectable_label(self.source.is_some(), "Markdown")
                    .on_hover_text("Show Markdown Source (Ctrl+Shift+M)")
                    .clicked()
                {
                    right.push(Action::ToggleSource);
                }
                ui.add_enabled_ui(editing, |ui| {
                    ui.separator();
                    if ui.button("⊞").on_hover_text("Insert Table").clicked() {
                        right.push(Action::InsertTable);
                    }
                    for (style, label, tip) in [
                        (BlockStyle::Task, "☑", "Checklist (Ctrl+Shift+L)"),
                        (BlockStyle::Ordered, "1.", "Numbered List (Ctrl+Shift+7)"),
                        (BlockStyle::Bullet, "•", "Bulleted List (Ctrl+Shift+8)"),
                    ] {
                        let on = editing && self.editor.block_at_cursor() == style;
                        if ui.selectable_label(on, label).on_hover_text(tip).clicked() {
                            right.push(Action::Block(style));
                        }
                    }
                    ui.separator();
                    for (style, label, tip) in [
                        (Inline::Code, RichText::new("<>").monospace(), "Code (Ctrl+E)"),
                        (Inline::Strike, RichText::new("S").strikethrough(), "Strikethrough (Ctrl+Shift+X)"),
                        (Inline::Italic, RichText::new("I").italics(), "Italic (Ctrl+I)"),
                        (Inline::Bold, RichText::new("B").strong(), "Bold (Ctrl+B)"),
                    ] {
                        let on = editing && self.editor.inline_active(&style);
                        if ui.selectable_label(on, label).on_hover_text(tip).clicked() {
                            right.push(Action::Inline(style));
                        }
                    }
                    ui.menu_button("Aa", |ui| {
                        for (style, label) in [
                            (BlockStyle::Heading(1), "Title"),
                            (BlockStyle::Heading(2), "Heading"),
                            (BlockStyle::Heading(3), "Subheading"),
                            (BlockStyle::Paragraph, "Body"),
                            (BlockStyle::Quote, "Quote"),
                            (BlockStyle::Code, "Monospaced"),
                        ] {
                            if ui.button(label).clicked() {
                                right.push(Action::Block(style));
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Paragraph Style");
                });
            },
        );
        for action in left.into_iter().chain(right) {
            self.act(ui.ctx(), action);
        }
    }

    fn act(&mut self, ctx: &egui::Context, action: Action) {
        let now = ctx.input(|i| i.time);
        match action {
            Action::PickFolder => self.pick_folder(),
            Action::NewNote => self.request_new_note(None),
            Action::NewFolder => self.request_new_folder(None),
            Action::RenameNote => {
                if let Some(note) = &self.note {
                    self.dialog =
                        Some(Dialog::Rename { name: notes::display_name(&note.path), path: note.path.clone() });
                    self.dialog_error = None;
                }
            }
            Action::Save => self.save(),
            Action::TrashNote => {
                if let Some(note) = &self.note {
                    self.dialog = Some(Dialog::Delete { path: note.path.clone() });
                }
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Undo => self.editor.undo(),
            Action::Redo => self.editor.redo(),
            Action::SelectAll => {
                self.editor.select_all();
                self.editor.focus(ctx);
            }
            Action::CopySkins => match skin::shared_dir().ok_or("No config folder".to_string()).and_then(|dir| {
                skin::export_built_ins(&dir, &[skin::BUILT_IN, BUILT_IN].concat())
            }) {
                Ok(export) if export.copied.is_empty() => {
                    self.notice = Some("The built-in skins are already in the skins folder".into())
                }
                Ok(export) => self.notice = Some(format!("Copied {} to the skins folder", export.copied.join(", "))),
                Err(e) => self.error = Some(format!("Could not copy skins: {e}")),
            },
            Action::OpenSkinsFolder => {
                if let Some(dir) = skin::shared_dir() {
                    let _ = std::fs::create_dir_all(&dir);
                    ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}", dir.display())));
                }
            }
            Action::SetSkin(name) => {
                self.forced_skin = None;
                self.config.skin = Some(name);
                self.config.save();
                self.apply_skin(ctx);
            }
            Action::ReloadSkins => {
                (self.skins, self.skin_errors) = load_skins();
                self.apply_skin(ctx);
            }
            Action::SystemTitleBar(on) => {
                self.config.system_title_bar = on;
                self.config.save();
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(on));
            }
            Action::ToggleSource => self.toggle_source(),
            Action::InsertTable => self.editor.insert_table(now),
            Action::Block(style) => {
                self.editor.set_block(style, now);
                self.editor.focus(ctx);
            }
            Action::Inline(style) => {
                self.editor.toggle_inline(style, now);
                self.editor.focus(ctx);
            }
        }
    }

    /// The menu bar: File, Edit, View and Skins.
    fn menus(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let has_folder = self.folder.is_some();
        let has_note = self.note.is_some();
        let editing = has_note && self.source.is_none();
        let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, keys: &str, action: Action| {
            if ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys)).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        ui.menu_button("File", |ui| {
            item(ui, true, "Open Folder…", "Ctrl+O", Action::PickFolder);
            ui.separator();
            item(ui, has_folder, "New Note…", "Ctrl+N", Action::NewNote);
            item(ui, has_folder, "New Folder…", "Ctrl+Shift+N", Action::NewFolder);
            ui.separator();
            item(ui, has_note, "Save", "Ctrl+S", Action::Save);
            item(ui, has_note, "Rename Note…", "F2", Action::RenameNote);
            item(ui, has_note, "Move Note to Trash…", "", Action::TrashNote);
            ui.separator();
            item(ui, true, "Quit", "Ctrl+Q", Action::Quit);
        });
        ui.menu_button("Edit", |ui| {
            item(ui, editing, "Undo", "Ctrl+Z", Action::Undo);
            item(ui, editing, "Redo", "Ctrl+Shift+Z", Action::Redo);
            ui.separator();
            item(ui, editing, "Select All", "Ctrl+A", Action::SelectAll);
            ui.separator();
            item(ui, editing, "Insert Table", "", Action::InsertTable);
        });
        ui.menu_button("View", |ui| {
            let mut source = self.source.is_some();
            if ui.add_enabled(has_note, egui::Checkbox::new(&mut source, "Markdown source")).changed() {
                actions.push(Action::ToggleSource);
            }
            let mut system = self.config.system_title_bar;
            if ui.checkbox(&mut system, "System title bar").changed() {
                actions.push(Action::SystemTitleBar(system));
            }
        });
        ui.menu_button("Skins", |ui| {
            let current = self.skin().name.clone();
            for skin in &self.skins {
                let resp = ui.radio(skin.name == current, &skin.name);
                let resp = match &skin.path {
                    Some(path) => resp.on_hover_text(path.display().to_string()),
                    None => resp,
                };
                if resp.clicked() {
                    actions.push(Action::SetSkin(skin.name.clone()));
                }
            }
            ui.separator();
            if ui.button("Reload skins").clicked() {
                actions.push(Action::ReloadSkins);
            }
            if ui.button("Copy built-in skins to folder").clicked() {
                actions.push(Action::CopySkins);
            }
            if ui.button("Open skins folder").clicked() {
                actions.push(Action::OpenSkinsFolder);
            }
            if !self.skin_errors.is_empty() {
                ui.separator();
                for error in &self.skin_errors {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let Some(folder) = self.folder.clone() else {
            ui.add_space(8.0);
            if let Some(note) = &self.note {
                let dir = note.path.parent().map(Path::to_path_buf);
                ui.label(RichText::new("This note is open on its own.").weak());
                if let Some(dir) = dir
                    && ui.button("Open Containing Folder").clicked()
                {
                    self.open_folder(dir);
                }
            }
            if let Some(remembered) = self.config.folder.clone().filter(|f| f.is_dir()) {
                let name = notes::display_name(&remembered);
                if ui.button(format!("Back to {name}")).clicked() {
                    self.open_folder(remembered);
                }
            }
            return;
        };
        let root_name = notes::display_name(&folder);
        let resp = ui.add(egui::Label::new(RichText::new(root_name).strong()).sense(egui::Sense::click()));
        resp.context_menu(|ui| self.folder_menu(ui, &folder));
        self.drop_target(ui, &resp, &folder);
        let entries = self.entries.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let new_note = ui
                .add(egui::Button::new(RichText::new("+ New Note").weak()).frame(false))
                .on_hover_text("New Note (Ctrl+N), in the open note's folder");
            if new_note.clicked() {
                self.request_new_note(None);
            }
            for entry in &entries {
                self.entry(ui, entry);
            }
            // Room below the tree for right-clicking and dropping at the top level.
            let rest = ui.available_size();
            let (_, resp) = ui.allocate_exact_size(egui::vec2(rest.x, rest.y.max(40.0)), egui::Sense::click());
            resp.context_menu(|ui| self.folder_menu(ui, &folder));
            self.drop_target(ui, &resp, &folder);
        });
    }

    /// Makes `resp` accept dragged notes and folders into `dir`,
    /// highlighting it while one hovers.
    fn drop_target(&mut self, ui: &egui::Ui, resp: &egui::Response, dir: &Path) {
        if resp.dnd_hover_payload::<PathBuf>().is_some() {
            let v = ui.visuals();
            ui.painter().rect_stroke(resp.rect.expand(1.0), 3.0, v.selection.stroke, egui::StrokeKind::Outside);
        }
        if let Some(from) = resp.dnd_release_payload::<PathBuf>() {
            self.drop_into(&from, dir);
        }
    }

    fn drop_into(&mut self, from: &Path, dir: &Path) {
        if from.parent() == Some(dir) {
            return;
        }
        let to = dir.join(from.file_name().unwrap_or_default());
        if let Err(e) = self.move_entry(from, to) {
            self.error = Some(format!("Could not move {}: {e}", notes::display_name(from)));
        }
    }

    fn entry(&mut self, ui: &mut egui::Ui, entry: &Entry) {
        match entry {
            Entry::Note { path, name } => {
                let current = self.note.as_ref().is_some_and(|n| &n.path == path);
                // One widget that both clicks and drags: a drag-source wrapper
                // around the label would sit on top of it and eat its clicks.
                let resp = ui.add(egui::Button::selectable(current, format!("📄 {name}")).sense(egui::Sense::click_and_drag()));
                resp.dnd_set_drag_payload(path.clone());
                if resp.clicked() {
                    self.open_note(path.clone());
                }
                resp.context_menu(|ui| self.item_menu(ui, path));
            }
            Entry::Folder { path, name, children } => {
                let id = ui.make_persistent_id(("folder", path));
                let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                let header = state.show_header(ui, |ui| {
                    let resp = ui.add(egui::Label::new(format!("📁 {name}")).sense(egui::Sense::click_and_drag()));
                    resp.dnd_set_drag_payload(path.clone());
                    resp
                });
                let (_, header, _) = header.body(|ui| {
                    for child in children {
                        self.entry(ui, child);
                    }
                });
                let row = header.inner;
                if row.clicked() {
                    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                    state.toggle(ui);
                    state.store(ui.ctx());
                }
                row.context_menu(|ui| self.folder_menu(ui, path));
                self.drop_target(ui, &row, path);
                // Resting a drag on a closed folder opens it.
                let dragging = egui::DragAndDrop::has_payload_of_type::<PathBuf>(ui.ctx());
                if dragging && row.contains_pointer() {
                    let mut state =
                        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                    if !state.is_open() {
                        state.set_open(true);
                        state.store(ui.ctx());
                    }
                }
            }
        }
    }

    fn item_menu(&mut self, ui: &mut egui::Ui, path: &Path) {
        if ui.button("Rename…").clicked() {
            self.dialog = Some(Dialog::Rename { name: notes::display_name(path), path: path.to_path_buf() });
            self.dialog_error = None;
            ui.close();
        }
        if ui.button("Move to Trash").clicked() {
            self.dialog = Some(Dialog::Delete { path: path.to_path_buf() });
            ui.close();
        }
    }

    fn folder_menu(&mut self, ui: &mut egui::Ui, dir: &Path) {
        if ui.button("New Note…").clicked() {
            self.request_new_note(Some(dir.to_path_buf()));
            ui.close();
        }
        if ui.button("New Folder…").clicked() {
            self.request_new_folder(Some(dir.to_path_buf()));
            ui.close();
        }
        if Some(dir) != self.folder.as_deref() {
            ui.separator();
            self.item_menu(ui, dir);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else { return };
        let mut action: Option<bool> = None;
        let title = match dialog {
            Dialog::NewNote { .. } => "New Note",
            Dialog::NewFolder { .. } => "New Folder",
            Dialog::Rename { .. } => "Rename",
            Dialog::Delete { .. } => "Move to Trash?",
        };
        let error = self.dialog_error.clone();
        let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(title);
            ui.add_space(8.0);
            match dialog {
                Dialog::NewNote { name, .. } | Dialog::NewFolder { name, .. } | Dialog::Rename { name, .. } => {
                    let resp = ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(f32::INFINITY));
                    if !resp.has_focus() && !resp.lost_focus() {
                        resp.request_focus();
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        action = Some(true);
                    }
                }
                Dialog::Delete { path } => {
                    ui.label(format!("“{}” will be moved to the trash.", notes::display_name(path)));
                }
            }
            if let Some(error) = &error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = if matches!(dialog, Dialog::Delete { .. }) { "Move to Trash" } else { "OK" };
                if ui.button(ok).clicked() {
                    action = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    action = Some(false);
                }
            });
        });
        if modal.should_close() && action.is_none() {
            action = Some(false);
        }
        match action {
            Some(false) => self.dialog = None,
            Some(true) => {
                let result = match self.dialog.take() {
                    Some(Dialog::NewNote { dir, name }) => {
                        let r = self.create_note(&dir, &name);
                        if r.is_ok() {
                            self.editor.focus(ctx);
                        }
                        r.map_err(|e| (Dialog::NewNote { dir, name }, e))
                    }
                    Some(Dialog::NewFolder { dir, name }) => {
                        self.create_folder(&dir, &name).map_err(|e| (Dialog::NewFolder { dir, name }, e))
                    }
                    Some(Dialog::Rename { path, name }) => {
                        self.rename(&path, &name).map_err(|e| (Dialog::Rename { path, name }, e))
                    }
                    Some(Dialog::Delete { path }) => self.delete(&path).map_err(|e| (Dialog::Delete { path }, e)),
                    None => Ok(()),
                };
                match result {
                    Ok(()) => self.dialog_error = None,
                    Err((dialog, e)) => {
                        self.dialog = Some(dialog);
                        self.dialog_error = Some(e);
                    }
                }
            }
            None => {}
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.heading("Notebook");
            ui.label(RichText::new("Open a folder of markdown notes to start.").weak());
            ui.add_space(12.0);
            if ui.button("Open Folder…").clicked() {
                self.pick_folder();
            }
        });
    }

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, requested)) = &mut self.screenshot else { return };
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            match crate::screenshot::save(&image, path) {
                Ok(()) => eprintln!("saved {}", path.display()),
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !*requested && ctx.input(|i| i.time) > 1.0 {
            *requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }
}

/// The app's own skins, compiled in.
const BUILT_IN: &[(&str, &str)] = &[("notebook.toml", include_str!("../skins/notebook.toml"))];

/// The skin used until one is picked: the app's own.
const HOME_SKIN: &str = "Notebook";

fn load_skins() -> (Vec<Skin>, Vec<String>) {
    let built_in = [skin::BUILT_IN, BUILT_IN].concat();
    skin::load_all(&built_in, &skin::shared_dir().into_iter().collect::<Vec<_>>())
}

/// Colours the title bar's own controls from the frame style when a skin
/// gives the bar a colour of its own (Notebook's brown), so menus and buttons
/// read on it. Popups and the rest of the window keep the skin's colours.
/// The same as Calendar's; it belongs in Patina's `TitleBar` (ADR-0009).
fn bar_controls(ui: &mut egui::Ui, style: &frame::FrameStyle) {
    let v = ui.visuals_mut();
    if style.title_bar == v.panel_fill {
        return;
    }
    let ink = style.title_text;
    v.override_text_color = Some(ink);
    v.weak_text_color = Some(ink.gamma_multiply(0.75));
    v.widgets.noninteractive.bg_stroke.color = ink.gamma_multiply(0.35);
    for (w, fill) in [
        (&mut v.widgets.inactive, ink.gamma_multiply(0.14)),
        (&mut v.widgets.hovered, style.controls.hover),
        (&mut v.widgets.active, style.controls.pressed),
        (&mut v.widgets.open, style.controls.pressed),
    ] {
        w.weak_bg_fill = fill;
        w.fg_stroke.color = ink;
    }
    v.selection.bg_fill = ink.gamma_multiply(0.26);
    v.selection.stroke.color = ink;
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(path) = self.picked.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.picked = None;
            self.open_folder(path);
        }
        if self.dialog.is_none() {
            self.shortcuts(ctx);
        }
        // Autosave a moment after the last edit.
        let now = ctx.input(|i| i.time);
        if self.dirty()
            && let Some(note) = &self.note
        {
            let wait = AUTOSAVE - (now - note.edited_at);
            if wait <= 0.0 {
                self.save();
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(wait));
            }
        }
        if self.picked.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let custom = !self.config.system_title_bar;
        let style = self.skin().frame_style(ui.visuals());
        let radius = if custom { frame::corner_radius(&ctx, style.radius) } else { 0 };

        egui::Panel::top("header")
            .frame(frame::header_frame(ui.style(), &style, radius))
            .show(ui, |ui| self.header(ui, &style));

        let message = self.error.clone().map(|e| (e, true)).or_else(|| self.notice.clone().map(|n| (n, false)));
        if let Some((text, is_error)) = message {
            egui::Panel::bottom("message").show(ui, |ui| {
                ui.horizontal(|ui| {
                    if is_error {
                        ui.colored_label(ui.visuals().error_fg_color, text);
                    } else {
                        ui.label(text);
                    }
                    if ui.small_button("Dismiss").clicked() {
                        self.error = None;
                        self.notice = None;
                    }
                });
            });
        }

        let show_sidebar = self.folder.is_some() || self.note.is_some();
        if show_sidebar {
            egui::Panel::left("sidebar")
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .corner_radius(egui::CornerRadius { sw: radius, ..Default::default() }),
                )
                .default_size(220.0)
                .show(ui, |ui| self.sidebar(ui));
        }

        let corner = egui::CornerRadius { se: radius, sw: if show_sidebar { 0 } else { radius }, ..Default::default() };
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).corner_radius(corner).inner_margin(0))
            .show(ui, |ui| {
                if self.note.is_none() {
                    if self.folder.is_some() {
                        ui.centered_and_justified(|ui| ui.label(RichText::new("No note open").weak()));
                    } else {
                        self.welcome(ui);
                    }
                    return;
                }
                let now = ui.input(|i| i.time);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    // A readable line length, centred.
                    let width = ui.available_width().min(760.0);
                    let margin = ((ui.available_width() - width) / 2.0).max(24.0);
                    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(margin as i8, 20)).show(ui, |ui| {
                        let changed = match &mut self.source {
                            Some(source) => ui
                                .add(
                                    egui::TextEdit::multiline(source)
                                        .code_editor()
                                        .frame(egui::Frame::NONE)
                                        .desired_width(f32::INFINITY),
                                )
                                .changed(),
                            None => self.editor.show(ui),
                        };
                        if changed {
                            if self.source.is_some() {
                                // The source has no edit count of its own.
                                if let Some(note) = &mut self.note {
                                    note.saved_edits = u64::MAX;
                                }
                            }
                            if let Some(note) = &mut self.note {
                                note.edited_at = now;
                            }
                        }
                    });
                });
            });

        self.dialogs(&ctx);
        if custom {
            frame::edges(&ctx, radius, style.border);
        }
        self.take_screenshot(&ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        frame::clear_color(visuals, !self.config.system_title_bar)
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        frame::release_grab(ctx, raw_input);
    }

    fn on_exit(&mut self) {
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    #[test]
    fn the_notebook_skin_is_built_in_and_parses_cleanly() {
        let skin = Skin::parse(BUILT_IN[0].1).unwrap();
        assert_eq!(skin.name, HOME_SKIN);
        assert!(skin.warnings.is_empty(), "{:?}", skin.warnings);
        let v = skin.visuals(egui::Visuals::light());
        assert_ne!(skin.frame_style(&v).title_bar, v.panel_fill, "the bar has its own colour");
    }

    #[test]
    fn bar_controls_only_change_a_coloured_bar() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let plain = frame::FrameStyle::from_visuals(ui.visuals());
            let before = ui.visuals().clone();
            bar_controls(ui, &plain);
            assert_eq!(*ui.visuals(), before, "a bar in the panel colour is left alone");
            let brown = frame::FrameStyle { title_bar: Color32::BROWN, title_text: Color32::WHITE, ..plain };
            bar_controls(ui, &brown);
            assert_eq!(ui.visuals().widgets.inactive.fg_stroke.color, Color32::WHITE);
            assert_eq!(ui.visuals().override_text_color, Some(Color32::WHITE));
        });
    }
}
