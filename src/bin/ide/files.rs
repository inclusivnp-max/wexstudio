//! Material 3 explorer operations, worker review/commit and buffer identity preservation.
use super::*;
use std::{collections::BTreeMap, time::Instant};
use wexandroidstudio::{
    file_ops::{self, Action, Outcome, Prepared, Template},
    material::{self as m, ButtonKind as Kind, Icon},
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    New,
    Folder,
    Move,
    Trash,
    Restore,
}
enum Reply {
    Prepared(Result<Prepared, String>),
    Applied(Result<(Outcome, project::ExplorerInventory), String>),
}
pub(super) struct FileUi {
    pub open: bool,
    pub selected: Option<(PathBuf, bool)>,
    pub inventory: project::ExplorerInventory,
    pub changed: Instant,
    mode: Mode,
    template: Template,
    name: String,
    location: String,
    package: String,
    source: PathBuf,
    pub(super) prepared: Option<Prepared>,
    pub(super) error: Option<String>,
    rx: Option<mpsc::Receiver<Reply>>,
    pub(super) busy: bool,
    removed: BTreeMap<PathBuf, (Document, EditState, usize)>,
}
impl Default for FileUi {
    fn default() -> Self {
        Self {
            open: false,
            selected: None,
            inventory: Default::default(),
            changed: Instant::now() - Duration::from_secs(60),
            mode: Mode::New,
            template: Template::KotlinClass,
            name: String::new(),
            location: String::new(),
            package: String::new(),
            source: PathBuf::new(),
            prepared: None,
            error: None,
            rx: None,
            busy: false,
            removed: Default::default(),
        }
    }
}
impl Workbench {
    fn file_mutation_idle(&self) -> bool {
        !self.git.open
            && !self.replace_blocked()
            && self.open_ui.pending.is_none()
            && self.save_ui.pending.is_none()
            && !self.building
            && !self.running
            && !self.debug.busy
            && self.debug.connection.is_none()
            && !self.preview.busy
            && !self.refactor.busy
            && !self.refactor.dialog
            && !self.projects.open
    }
    pub(super) fn show_file_dialog(&mut self, mode: Mode, selected: Option<(PathBuf, bool)>) {
        if !self.file_mutation_idle() || self.file_ui.busy {
            self.status = "Finish Build / Run / Debug / Preview before changing paths".into();
            return;
        }
        let (path, directory) = selected
            .or_else(|| self.file_ui.selected.clone())
            .unwrap_or_else(|| (self.doc.path.clone().unwrap_or(self.root.clone()), false));
        let dir = if directory {
            path.clone()
        } else {
            path.parent().unwrap_or(&self.root).to_owned()
        };
        self.file_ui.source = path.strip_prefix(&self.root).unwrap_or(&path).to_owned();
        self.file_ui.location = dir
            .strip_prefix(&self.root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .into();
        self.file_ui.name = if mode == Mode::Move {
            self.file_ui.source.to_string_lossy().into()
        } else {
            String::new()
        };
        self.file_ui.package = String::new();
        let mut longest = 0;
        for (source, namespace) in &self.file_ui.inventory.package_roots {
            if let Ok(relative) = dir.strip_prefix(source)
                && source.components().count() >= longest
            {
                longest = source.components().count();
                self.file_ui.package = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(".");
                if self.file_ui.package.is_empty() {
                    self.file_ui.package = namespace.clone();
                }
            }
        }
        self.file_ui.mode = mode;
        self.file_ui.template = if mode == Mode::Folder {
            Template::Folder
        } else {
            Template::KotlinClass
        };
        self.file_ui.prepared = None;
        self.file_ui.error = None;
        self.file_ui.open = true;
    }
    fn file_action(&self) -> anyhow::Result<Action> {
        Ok(match self.file_ui.mode {
            Mode::New | Mode::Folder => {
                let (name, contents) = self
                    .file_ui
                    .template
                    .render(&self.file_ui.name, &self.file_ui.package)?;
                Action::Create {
                    path: PathBuf::from(&self.file_ui.location).join(name),
                    contents,
                }
            }
            Mode::Move => Action::Move {
                from: self.file_ui.source.clone(),
                to: PathBuf::from(&self.file_ui.name),
            },
            Mode::Trash => Action::Trash {
                path: self.file_ui.source.clone(),
            },
            Mode::Restore => Action::Restore,
        })
    }
    fn affected_paths(&self) -> Vec<PathBuf> {
        if !matches!(self.file_ui.mode, Mode::Move | Mode::Trash) {
            return vec![];
        }
        let p = self.root.join(&self.file_ui.source);
        self.tabs
            .iter()
            .filter(|f| f.starts_with(&p))
            .cloned()
            .collect()
    }
    pub(super) fn file_review_after_save(&mut self, signature: &str) {
        if self.file_ui.open
            && self
                .file_action()
                .is_ok_and(|a| format!("{a:?}") == signature)
        {
            self.file_review();
        }
    }
    fn file_review(&mut self) {
        let paths = self.affected_paths();
        for p in &paths {
            if self.editor_ui.conflicts.contains_key(p) {
                self.file_ui.error =
                    Some("Review external changes before moving this open buffer".into());
                return;
            }
        }
        let dirty = paths
            .iter()
            .filter(|p| self.dirty_path(p))
            .cloned()
            .collect::<Vec<_>>();
        if !dirty.is_empty() {
            match self.file_action() {
                Ok(a) => {
                    self.request_save_paths(dirty, save_ui::After::Files(format!("{a:?}")));
                    if self.save_ui.pending.is_some() {
                        self.file_ui.busy = true;
                    } else {
                        self.file_ui.error = Some(self.status.clone());
                    }
                }
                Err(e) => self.file_ui.error = Some(e.to_string()),
            };
            return;
        }
        let expected = paths
            .iter()
            .filter_map(|p| {
                let d = if self.doc.path.as_ref() == Some(p) {
                    Some(&self.doc)
                } else {
                    self.parked.get(p).map(|(d, _)| d)
                };
                d.and_then(|d| d.disk_hash()).map(|h| (p.clone(), h))
            })
            .collect::<Vec<_>>();
        let action = match self.file_action() {
            Ok(a) => a,
            Err(e) => {
                self.file_ui.error = Some(e.to_string());
                return;
            }
        };
        let root = self.root.clone();
        let wake = self.context.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.file_ui.rx = Some(rx);
        self.file_ui.busy = true;
        self.file_ui.error = None;
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                for (p, h) in expected {
                    anyhow::ensure!(
                        blake3::hash(&std::fs::read(p)?) == h,
                        "Open file changed on disk; review external change"
                    );
                }
                file_ops::prepare(&root, action)
            })();
            let _ = tx.send(Reply::Prepared(result.map_err(|e| format!("{e:#}"))));
            wake.request_repaint();
        });
    }
    fn file_commit(&mut self) {
        if !self.file_mutation_idle() {
            return;
        }
        let Some(prepared) = self.file_ui.prepared.take() else {
            return;
        };
        let root = self.root.clone();
        let wake = self.context.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.file_ui.rx = Some(rx);
        self.file_ui.busy = true;
        self.file_ui.error = None;
        std::thread::spawn(move || {
            let result = file_ops::apply(&root, prepared)
                .map(|o| {
                    let tree = project::explorer_entries(&root);
                    (o, tree)
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Reply::Applied(result));
            wake.request_repaint();
        });
    }
    pub(super) fn file_events(&mut self) {
        let Some(rx) = &self.file_ui.rx else {
            return;
        };
        let reply = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.file_ui.busy = false;
                self.file_ui.rx = None;
                self.file_ui.error = Some("File worker stopped · retry review".into());
                return;
            }
        };
        self.file_ui.rx = None;
        self.file_ui.busy = false;
        match reply {
            Reply::Prepared(Ok(p)) => self.file_ui.prepared = Some(p),
            Reply::Prepared(Err(e)) | Reply::Applied(Err(e)) => self.file_ui.error = Some(e),
            Reply::Applied(Ok((outcome, tree))) => {
                self.file_ui.changed = Instant::now();
                self.files = tree.sources();
                self.file_ui.inventory = tree;
                self.rebind_file_buffers(&outcome);
                self.file_ui.open = false;
                self.file_ui.selected = outcome.to.clone().map(|p| {
                    (
                        p.clone(),
                        self.file_ui
                            .inventory
                            .entries
                            .iter()
                            .any(|e| e.path == p && e.directory),
                    )
                });
                self.status = if outcome.trashed {
                    "Moved to project trash · Restore is available"
                } else {
                    "File operation completed · updating K2"
                }
                .into();
                self.last_sent = None;
                self.last_buffers.clear();
                self.refactor.plan = None;
                self.refactor.capture = None;
                self.refactor.usages.clear();
                self.preview.catalog = Default::default();
                self.diagnostics.clear();
                self.diagnostic_snapshot = None;
                self.completions.clear();
                self.editor_switched();
                self.sync_watches();
                self.send_snapshot();
                if self.language.is_some() {
                    self.connect_language(true);
                }
            }
        }
    }
    fn rebind_file_buffers(&mut self, o: &Outcome) {
        if self.debug.bp_service.active() {
            if let (Some(from), Some(to)) = (&o.from, &o.to) {
                self.breakpoint_request(Some(wexandroidstudio::breakpoint_store::Action::Remap(
                    from.clone(),
                    to.clone(),
                )));
            } else {
                self.breakpoint_request(Some(wexandroidstudio::breakpoint_store::Action::Refresh));
            }
        }
        if let (Some(from), Some(to)) = (&o.from, &o.to) {
            let remap = |p: &PathBuf| {
                p.strip_prefix(from)
                    .ok()
                    .map(|r| {
                        if r.as_os_str().is_empty() {
                            to.clone()
                        } else {
                            to.join(r)
                        }
                    })
                    .unwrap_or_else(|| p.clone())
            };
            if let Some(p) = self.doc.path.as_mut() {
                *p = remap(p);
            }
            self.tabs = self.tabs.iter().map(&remap).collect();
            self.parked = std::mem::take(&mut self.parked)
                .into_iter()
                .map(|(p, (mut d, e))| {
                    let p = remap(&p);
                    d.path = Some(p.clone());
                    (p, (d, e))
                })
                .collect();
            self.chrome.collapsed = self.chrome.collapsed.iter().map(&remap).collect();
            self.editor_ui.conflicts.retain(|p, _| !p.starts_with(from));
            self.debug.breakpoints = std::mem::take(&mut self.debug.breakpoints)
                .into_iter()
                .map(|(p, v)| (remap(&p), v))
                .collect();
        } else if let Some(from) = &o.from {
            let affected = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.starts_with(from))
                .map(|(i, p)| (i, p.clone()))
                .collect::<Vec<_>>();
            for (i, p) in affected {
                let doc = if self.doc.path.as_ref() == Some(&p) {
                    Some((&self.doc, &self.edit))
                } else {
                    self.parked.get(&p).map(|(d, e)| (d, e))
                };
                if let Some((d, e)) = doc
                    && let Ok(d) = d.fork()
                {
                    self.file_ui.removed.insert(p.clone(), (d, e.clone(), i));
                }
                self.close_tab(p, true);
            }
        } else if let Some(to) = &o.to {
            let restored = self
                .file_ui
                .removed
                .keys()
                .filter(|p| p.starts_with(to))
                .cloned()
                .collect::<Vec<_>>();
            for p in &restored {
                let (d, e, i) = self.file_ui.removed.remove(p).unwrap();
                if !self.tabs.contains(p) {
                    self.tabs.insert(i.min(self.tabs.len()), p.clone());
                    self.parked.insert(p.clone(), (d, e));
                }
            }
            let directory = self
                .file_ui
                .inventory
                .entries
                .iter()
                .any(|e| e.path == *to && e.directory);
            if !directory {
                self.open_file(to.clone());
            }
        }
    }
    pub(super) fn file_toolbar(&mut self, ui: &mut egui::Ui) {
        let enabled = self.file_mutation_idle() && !self.file_ui.open;
        let reduced = self.reduced_motion;
        ui.horizontal(|ui| {
            for (key, label, icon, mode) in [
                ("files-new", "New file", Icon::Code, Mode::New),
                ("files-folder", "New folder", Icon::Folder, Mode::Folder),
                (
                    "files-move",
                    "Rename / Move selected",
                    Icon::More,
                    Mode::Move,
                ),
                ("files-trash", "Trash selected", Icon::Close, Mode::Trash),
                (
                    "files-restore",
                    "Restore last trashed item",
                    Icon::Refresh,
                    Mode::Restore,
                ),
            ] {
                if self
                    .chrome
                    .button(ui, key, "", icon, Kind::Ghost, enabled, 32., reduced)
                    .on_hover_text_passive(label)
                    .clicked()
                {
                    self.show_file_dialog(mode, None);
                }
            }
        });
    }
    pub(super) fn file_dialog(&mut self, ctx: &egui::Context) {
        if !self.file_ui.open {
            return;
        }
        let mut review = false;
        let mut commit = false;
        let mut close = false;
        let reduced = self.reduced_motion;
        egui::Modal::new(egui::Id::new("file-operation")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui|{
            ui.set_width(520.);ui.heading(match self.file_ui.mode {Mode::New=>"Create a file",Mode::Folder=>"Create a folder",Mode::Move=>"Rename / Move",Mode::Trash=>"Project trash",Mode::Restore=>"Restore from trash"});ui.add_space(12.);
            if let Some(p)=&self.file_ui.prepared {
                ui.label(RichText::new(p.summary()).color(m::PRIMARY).size(18.));ui.add_space(12.);ui.label("The reviewed source is checked again at commit. Existing destinations are never overwritten.");
                if self.chrome.button(ui,"files-commit","Confirm",Icon::Check,Kind::Filled,!self.file_ui.busy,150.,reduced).clicked(){commit=true;}
            } else {
                ui.add_enabled_ui(!self.file_ui.busy,|ui|{
                    match self.file_ui.mode {
                        Mode::New|Mode::Folder=>{
                            if self.file_ui.mode==Mode::New {ui.horizontal_wrapped(|ui|{for t in Template::ALL {if t==Template::Folder{continue;}let key=format!("files-kind-{}",t.label());if self.chrome.button(ui,&key,t.label(),Icon::Code,if self.file_ui.template==t{Kind::Filled}else{Kind::Tonal},true,140.,reduced).clicked(){self.file_ui.template=t;}}});}
                            field(ui,&mut self.chrome.hits,"files-name","Name (source templates add the extension)",&mut self.file_ui.name);
                            field(ui,&mut self.chrome.hits,"files-location","Existing folder · relative to project (empty = root)",&mut self.file_ui.location);
                            if matches!(self.file_ui.template,Template::KotlinClass|Template::JavaClass|Template::KotlinFile){field(ui,&mut self.chrome.hits,"files-package","Package",&mut self.file_ui.package);}
                            if let Ok((name,contents))=self.file_ui.template.render(&self.file_ui.name,&self.file_ui.package){ui.label(format!("{}/{}",self.file_ui.location,name));if let Some(text)=contents {ui.label(RichText::new(text).monospace().size(12.).color(m::MUTED));}}
                        },Mode::Move=>{ui.label(format!("From: {}",self.file_ui.source.display()));field(ui,&mut self.chrome.hits,"files-name","Destination · complete project-relative path",&mut self.file_ui.name);ui.label("Path-only operation: class names, packages and references stay unchanged. Use K2 Rename symbol for code refactoring.");},
                        Mode::Trash=>{ui.label(format!("{}",self.file_ui.source.display()));ui.label("Files move into .wex/file-history, not permanent deletion. Restore refuses collisions and changed payloads.");},
                        Mode::Restore=>{ui.label("Restore the newest retained item to its original path. Trash history survives closing the IDE.");},
                    }
                    let dirty=self.affected_paths().iter().filter(|p|self.dirty_path(p)).count();
                    if dirty>0 {ui.label(RichText::new(format!("{dirty} affected unsaved tabs · save these before review. Unrelated unsaved tabs stay untouched.")).color(m::WARNING));}
                    if self.chrome.button(ui,"files-review",if dirty>0{"Save affected & review"}else{"Review operation"},Icon::Check,Kind::Filled,true,235.,reduced).clicked(){review=true;}
                });
            }
            if self.save_ui.pending.is_some() && self.chrome.button(ui,"files-save-stop","Stop remaining saves",Icon::Close,Kind::Outline,true,220.,reduced).clicked(){self.stop_remaining_saves();}
            if self.file_ui.busy {ui.label(if self.save_ui.pending.is_some(){"Saving affected snapshots…"}else{"Checking files on a worker…"});ctx.request_repaint_after(Duration::from_millis(80));}
            if let Some(e)=&self.file_ui.error {ui.label(RichText::new(e).color(m::ERROR));}
            ui.add_space(10.);if self.chrome.button(ui,"files-cancel","Cancel",Icon::Close,Kind::Ghost,!self.file_ui.busy,110.,reduced).clicked(){close=true;}
        });
        if !self.file_ui.busy
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            close = true;
        }
        if close {
            self.file_ui.open = false;
            self.file_ui.prepared = None;
        } else if review {
            self.file_review();
        } else if commit {
            self.file_commit();
        }
    }
}
fn field(
    ui: &mut egui::Ui,
    hits: &mut BTreeMap<String, egui::Rect>,
    key: &str,
    label: &str,
    value: &mut String,
) {
    ui.label(label);
    let r = ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(key)
            .desired_width(ui.available_width())
            .margin(egui::vec2(12., 10.)),
    );
    hits.insert(key.into(), r.rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moved_file_has_no_trailing_slash_and_retains_undo_and_parked_dirty() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A.kt");
        let b = d.path().join("B.kt");
        std::fs::write(&a, "val a=1\n").unwrap();
        let mut app = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        app.doc.edit(0..0, "// work\n").unwrap();
        app.doc.save().unwrap();
        let p = d.path().join("Unrelated.kt");
        let mut parked = Document::new("val unsaved=2").unwrap();
        parked.path = Some(p.clone());
        parked.edit(0..0, "// dirty\n").unwrap();
        app.tabs.push(p.clone());
        app.parked.insert(p.clone(), (parked, Default::default()));
        std::fs::rename(&a, &b).unwrap();
        app.rebind_file_buffers(&Outcome {
            from: Some(a),
            to: Some(b.clone()),
            trashed: false,
        });
        assert_eq!(app.doc.path.as_ref().unwrap().as_os_str(), b.as_os_str());
        assert_eq!(
            std::fs::read_to_string(app.doc.path.as_ref().unwrap()).unwrap(),
            "// work\nval a=1\n"
        );
        assert!(app.doc.undo().unwrap());
        assert_eq!(app.doc.rope().to_string(), "val a=1\n");
        assert!(app.parked[&p].0.dirty());
    }
    #[test]
    fn config_tab_keeps_dirty_source_overlay_and_last_source_close_clears_k2_uri() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("A.kt");
        let config = d.path().join("notes.txt");
        std::fs::write(&source, "val a=1\n").unwrap();
        std::fs::write(&config, "notes").unwrap();
        let mut app = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        app.doc.edit(0..0, "// dirty\n").unwrap();
        let first = app.language_snapshot().unwrap().unwrap();
        app.open_file_wait(config);
        let parked = app.language_snapshot().unwrap().unwrap();
        assert_eq!(first.text, parked.text);
        assert_eq!(parked.path, source);
        assert!(parked.version > first.version);
        assert!(app.language_snapshot().is_none());
        app.close_tab(source, true);
        assert!(app.language_snapshot().unwrap().is_none());
        assert!(app.language_snapshot().is_none());
    }
    #[test]
    fn modal_blocks_build_run_debug_editor_and_semantic_shortcuts() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "val a=1\n").unwrap();
        let mut app = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        app.show_file_dialog(Mode::Move, None);
        app.build();
        app.run();
        app.start_debug();
        app.start_preview();
        assert!(!app.building && !app.running && !app.debug.busy && !app.preview.busy);
        assert!(app.file_ui.open);
    }
    #[test]
    fn missing_clean_parked_source_closes_overlay_without_losing_buffer_or_draft() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("A.kt");
        let gone = d.path().join("Gone.kt");
        std::fs::write(&main, "val a=1\n").unwrap();
        std::fs::write(&gone, "class Gone\n").unwrap();
        let mut a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        assert!(a.open_file_wait(gone.clone()));
        assert!(a.open_file_wait(main.clone()));
        let before = a.language_snapshot().unwrap().unwrap();
        assert!(before.others.iter().any(|b| b.path == gone));
        assert!(a.language_snapshot().is_none());
        std::fs::remove_file(&gone).unwrap();
        a.editor_ui.conflicts.insert(
            gone.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        let next=a.language_snapshot().expect("observed deletion changes the published source set even when Rope clocks did not change").unwrap();
        assert!(!next.others.iter().any(|b| b.path == gone));
        assert!(a.parked.contains_key(&gone));
        assert_eq!(a.parked[&gone].0.rope().to_string(), "class Gone\n");
        assert!(!a.parked[&gone].0.dirty());
        assert!(a.language_snapshot().is_none());
        a.parked
            .get_mut(&gone)
            .unwrap()
            .0
            .edit(0..0, "// intentional unsaved draft\n")
            .unwrap();
        let next = a.language_snapshot().unwrap().unwrap();
        assert!(next.others.iter().any(|b| b.path == gone));
        assert!(!gone.exists());
    }
    #[test]
    fn missing_clean_active_source_clears_k2_until_an_explicit_draft_or_disk_restore() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("A.kt");
        std::fs::write(&path, "val a=1\n").unwrap();
        let mut a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        assert!(a.language_snapshot().unwrap().is_some());
        std::fs::remove_file(&path).unwrap();
        a.editor_ui.conflicts.insert(
            path.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        assert!(
            a.language_snapshot()
                .expect("close removed active URI")
                .is_none()
        );
        assert!(a.language_snapshot().is_none());
        a.request_language(false);
        assert!(!a.completion_pending);
        std::fs::write(&path, "val a=1\n").unwrap();
        a.editor_ui.conflicts.remove(&path);
        assert!(a.language_snapshot().unwrap().is_some());
        assert!(!a.doc.dirty());
    }
    #[test]
    fn definition_reply_cannot_navigate_behind_git_review() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        std::fs::write(&p, "val a=1\n").unwrap();
        let mut a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        let n = a.open_ui.navigation;
        let v = a.doc.version;
        assert!(a.definition_is_current(n, &p, v));
        a.git.open = true;
        assert!(
            !a.definition_is_current(n, &p, v),
            "an old asynchronous F12 must not move documents behind Git/branch review"
        );
    }
    #[test]
    fn actual_watcher_missing_retires_pending_semantics_without_erasing_draft_or_buffer() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        std::fs::write(&p, "val a=1\n").unwrap();
        let mut a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        a.language_snapshot();
        a.refactor.id = 5;
        a.refactor.busy = true;
        a.refactor.dialog = true;
        a.refactor.name = "draftName".into();
        a.refactor.origin = Some((p.clone(), 0, 0));
        a.completion_pending = true;
        a.diagnostic_snapshot = Some((p.clone(), a.doc.version));
        let hash = a.doc.content_hash();
        let navigation = a.open_ui.navigation;
        a.sync_watches();
        std::fs::remove_file(&p).unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(6);
        let ctx = a.context.clone();
        while !matches!(
            a.editor_ui.conflicts.get(&p),
            Some(wexandroidstudio::editor_jobs::DiskState::Missing)
        ) {
            assert!(std::time::Instant::now() < end);
            a.editor_updates(&ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.refactor.id, 6);
        assert!(!a.refactor.busy && a.refactor.origin.is_none());
        assert_eq!(a.refactor.name, "draftName");
        assert!(a.refactor.error.contains("Source removed"));
        assert!(!a.completion_pending);
        assert!(a.diagnostic_snapshot.is_none());
        assert!(!a.source_event_current(&p, a.doc.version));
        assert!(a.open_ui.navigation > navigation);
        assert_eq!(a.doc.content_hash(), hash);
        assert!(!a.doc.dirty());
        assert!(!p.exists());
    }
}
