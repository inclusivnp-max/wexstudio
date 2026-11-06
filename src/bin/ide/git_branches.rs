//! Local branches share the existing Git modal/worker. Selection never checks out;
//! Switch requires a second explicit review and rejects all unsaved editor tabs.
use super::*;
use std::ffi::OsString;
use vcs::branches::{Action, Catalog};
#[derive(Default)]
pub(crate) struct BranchUi {
    pub open: bool,
    pub return_focus: Option<egui::Id>,
    pub catalog: Option<Arc<Catalog>>,
    pub selected: Option<OsString>,
    pub filter: String,
    pub name: String,
    pub confirm: bool,
    focus_filter: bool,
    focus_list: bool,
    reveal: bool,
    ime_field: Option<egui::Id>,
    composing: bool,
}
impl Workbench {
    pub(super) fn git_branches_open(&mut self) {
        if self.git.busy || self.git.saving.is_some() {
            return;
        }
        self.ime_cancel_external();
        self.git.row_click = None;
        self.git.focus_list = false;
        self.git.branch_ui.open = true;
        self.git.branch_ui.focus_filter = true;
        self.git.branch_ui.confirm = false;
        self.git_refresh();
    }
    pub(super) fn git_branch_received(&mut self, c: Arc<Catalog>) {
        if self
            .git
            .branch_ui
            .catalog
            .as_ref()
            .is_none_or(|old| !old.same_review(&c))
        {
            self.git.branch_ui.confirm = false;
        }
        if self
            .git
            .branch_ui
            .selected
            .as_ref()
            .is_none_or(|s| !c.items.iter().any(|b| &b.name == s))
        {
            self.git.branch_ui.selected = c
                .items
                .iter()
                .find(|b| b.current)
                .or_else(|| c.items.first())
                .map(|b| b.name.clone());
        }
        self.git.snapshot = Some(c.snapshot.clone());
        self.git.branch_ui.catalog = Some(c);
        self.git.stale = false;
    }
    fn git_branch_unsaved(&self) -> usize {
        usize::from(self.doc.dirty()) + self.parked.values().filter(|(d, _)| d.dirty()).count()
    }
    pub(super) fn git_branch_apply(&mut self, action: Action) {
        if self.git.busy || self.git.stale || self.git.saving.is_some() {
            return;
        }
        if matches!(action, Action::Switch(_)) && self.git_branch_unsaved() > 0 {
            self.git.note =
                "Switch waits for Save or Undo of every unsaved tab · no implicit Save".into();
            return;
        }
        let Some(c) = self.git.branch_ui.catalog.clone() else {
            return;
        };
        self.git.branch_ui.confirm = false;
        self.git_start(Task::Branch(c, action), "Updating local branch…");
    }
    fn git_branches_back(&mut self, ctx: &egui::Context) {
        if self.git.mutating {
            return;
        }
        self.ime_cancel_external();
        self.git.branch_ui.open = false;
        self.git.branch_ui.confirm = false;
        if let Some(id) = self.git.branch_ui.return_focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        self.git_refresh();
    }
    fn git_branch_ime(&mut self, ctx: &egui::Context) -> bool {
        let was = self.git.branch_ui.composing;
        let field = ctx.memory(|m| m.focused()).filter(|id| {
            *id == egui::Id::new("git-branch-name") || *id == egui::Id::new("git-branch-filter")
        });
        if field != self.git.branch_ui.ime_field {
            self.git.branch_ui.ime_field = field;
            self.git.branch_ui.composing = false;
        }
        let mut incoming = false;
        ctx.input(|i| {
            if !i.focused {
                self.git.branch_ui.composing = false;
            }
            for e in &i.events {
                if let egui::Event::Ime(e) = e {
                    incoming = true;
                    match e {
                        egui::ImeEvent::Preedit(s) if !s.is_empty() => {
                            self.git.branch_ui.composing = true
                        }
                        egui::ImeEvent::Enabled
                        | egui::ImeEvent::Commit(_)
                        | egui::ImeEvent::Disabled => self.git.branch_ui.composing = false,
                        _ => {}
                    }
                }
            }
        });
        was || incoming || self.git.branch_ui.composing
    }
    fn git_branch_keys(&mut self, ctx: &egui::Context, rows: &[usize], body: f32) {
        let list = egui::Id::new("git-branch-list-focus");
        if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F6) {
            self.ime_cancel_external();
            if ctx.memory(|m| m.has_focus(list)) {
                self.git.branch_ui.focus_filter = true;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("git-branch-filter")));
            } else {
                self.git.branch_ui.focus_list = true;
                ctx.memory_mut(|m| m.request_focus(list));
            }
        }
        if !ctx.memory(|m| m.has_focus(list)) || self.git.mutating {
            return;
        }
        for event in ctx.input(|i| i.events.clone()) {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if !modifiers.is_none() {
                continue;
            }
            if key == Key::Enter {
                Self::consume_exact_key(ctx, egui::Modifiers::NONE, key);
                self.git.branch_ui.confirm = true;
                continue;
            }
            if !matches!(
                key,
                Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End | Key::PageUp | Key::PageDown
            ) || rows.is_empty()
            {
                continue;
            }
            Self::consume_exact_key(ctx, egui::Modifiers::NONE, key);
            let c = self.git.branch_ui.catalog.as_ref().unwrap();
            let old = rows
                .iter()
                .position(|n| Some(&c.items[*n].name) == self.git.branch_ui.selected.as_ref());
            let page = (body / 34.).floor().max(1.) as usize;
            let n = match key {
                Key::Home => 0,
                Key::End => rows.len() - 1,
                Key::ArrowUp => old.unwrap_or(1).saturating_sub(1),
                Key::ArrowDown => old.map_or(0, |n| n + 1),
                Key::PageUp => old.unwrap_or(page).saturating_sub(page),
                Key::PageDown => old.map_or(0, |n| n + page),
                _ => unreachable!(),
            }
            .min(rows.len() - 1);
            self.git.branch_ui.selected = Some(c.items[rows[n]].name.clone());
            self.git.branch_ui.confirm = false;
            self.git.branch_ui.reveal = true;
        }
    }
    pub(super) fn git_branches_dialog(&mut self, ctx: &egui::Context) {
        let ime = self.git_branch_ime(ctx);
        if !ime
            && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Escape)
            && !self.git.mutating
        {
            self.git_branches_back(ctx);
            return;
        }
        let width = (ctx.content_rect().width() - 80.).clamp(600., 880.);
        let extra = if self.git.details && !self.git.error.is_empty() {
            60.
        } else {
            0.
        } + if self.git.branch_ui.confirm { 40. } else { 0. };
        let body = (ctx.content_rect().height() - 385. - extra).clamp(40., 330.);
        let catalog = self.git.branch_ui.catalog.clone();
        let unsaved = self.git_branch_unsaved();
        let ready = !self.git.busy && !self.git.stale && self.git.saving.is_none();
        let selected = catalog.as_ref().and_then(|c| {
            c.items
                .iter()
                .position(|b| self.git.branch_ui.selected.as_ref() == Some(&b.name))
        });
        let can_switch = ready
            && unsaved == 0
            && selected.is_some_and(|n| {
                let b = &catalog.as_ref().unwrap().items[n];
                !b.current && b.worktree.is_none()
            });
        egui::Modal::new(egui::Id::new("git-review")).frame(m::card().inner_margin(20)).show(ctx,|ui|{
            ui.spacing_mut().item_spacing.y=4.;ui.set_width(width);
            ui.heading("Local branches");
            let branch=self.git.snapshot.as_ref().map_or("Reading repository…",|s|s.branch.as_str());
            let(r,_)=ui.allocate_exact_size(egui::vec2(width,18.),egui::Sense::hover());m::label_clipped(ui,r,&format!("Current: {branch}"),13.,m::PRIMARY);
            let message=if unsaved>0{format!("{unsaved} unsaved tabs · Save or Undo before Switch. Create keeps buffers.")}else{"Git preserves compatible local changes; collisions stop without force or stash.".into()};
            let(r,_)=ui.allocate_exact_size(egui::vec2(width,18.),egui::Sense::hover());m::label_clipped(ui,r,&message,11.,if unsaved>0{m::WARNING}else{m::MUTED});ui.interact(r,ui.id().with("branch-help"),egui::Sense::hover()).on_hover_text_passive(message);
            let r=ui.add_enabled(!self.git.mutating,egui::TextEdit::singleline(&mut self.git.branch_ui.filter).id(egui::Id::new("git-branch-filter")).desired_width(width).hint_text("Find a local branch… · F6 moves to list").char_limit(4096));self.chrome.hits.insert("git-branch-filter".into(),r.rect);
            if self.git.branch_ui.focus_filter{r.request_focus();self.git.branch_ui.focus_filter=ui.is_sizing_pass();}
            if r.changed(){self.git.branch_ui.confirm=false;}
            let filter=self.git.branch_ui.filter.to_lowercase();let rows=catalog.as_ref().map(|c|c.items.iter().enumerate().filter(|(_,b)|b.name.to_string_lossy().to_lowercase().contains(&filter)).map(|(n,_)|n).collect::<Vec<_>>()).unwrap_or_default();
            let row_body=body.min(rows.len().max(1)as f32*34.);
            if !ime {self.git_branch_keys(ctx,&rows,row_body);}
            let list=ui.interact(egui::Rect::from_min_size(ui.cursor().min,egui::vec2(width,row_body)),egui::Id::new("git-branch-list-focus"),egui::Sense::focusable_noninteractive());
            self.chrome.hits.insert("git-branch-list-viewport".into(),list.rect);
            list.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Other,true,"Local Git branches · F6 filter/list; arrows navigate; Enter reviews only"));
            if self.git.branch_ui.focus_list {list.request_focus();self.git.branch_ui.focus_list=ui.is_sizing_pass();}
            let mut scroll=egui::ScrollArea::vertical().id_salt("git-branch-list");
            if std::mem::take(&mut self.git.branch_ui.reveal)&&let Some(n)=rows.iter().position(|at|catalog.as_ref().is_some_and(|c|Some(&c.items[*at].name)==self.git.branch_ui.selected.as_ref())){scroll=scroll.vertical_scroll_offset((n as f32*34.+17.-row_body*0.5).max(0.));}
            scroll.min_scrolled_height(row_body).max_height(row_body).auto_shrink([false,false]).show_rows(ui,34.,rows.len().max(1),|ui,range|{
                ui.spacing_mut().item_spacing.y=0.;
                if rows.is_empty(){ui.label(if catalog.is_none(){"Reading local branches…"}else{"No matching local branches"});}
                for n in range.filter(|n|*n<rows.len()){
                    let at=rows[n];let b=&catalog.as_ref().unwrap().items[at];
                    let(r,_)=ui.allocate_exact_size(egui::vec2(width-14.,34.),egui::Sense::hover());
                    let response=ui.interact(r,ui.id().with((&b.name,"branch-row")),egui::Sense::click());
                    let picked=self.git.branch_ui.selected.as_ref()==Some(&b.name);
                    response.widget_info(||egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel,true,picked,b.name.to_string_lossy()));
                    self.chrome.hits.insert(format!("git-branch-{at}"),r);
                    if picked{ui.painter().rect_filled(r,10,m::PRIMARY_CONTAINER);}
                    let label=format!("{}{}  ·  {}{}",if b.current{"● "}else{""},b.name.to_string_lossy(),&b.oid[..8],if b.worktree.is_some()&&!b.current{" · in another worktree"}else{""});
                    m::label_clipped(ui,r.shrink2(egui::vec2(8.,0.)),&label,13.,if ready{m::TEXT}else{m::MUTED});m::passive_tooltip(&response,format!("{}\n{}{}",b.name.to_string_lossy(),b.oid,b.worktree.as_ref().map(|p|format!("\nWorktree: {}",p.display())).unwrap_or_default()));
                    if response.clicked()&&!self.git.mutating{self.git.branch_ui.selected=Some(b.name.clone());self.git.branch_ui.confirm=false;ctx.memory_mut(|m|m.request_focus(egui::Id::new("git-branch-list-focus")));}
                }
            });
            ui.label(RichText::new("Create at reviewed HEAD · stay on the current branch").small().color(m::MUTED));
            ui.horizontal(|ui|{
                let r=ui.add_enabled(!self.git.mutating,egui::TextEdit::singleline(&mut self.git.branch_ui.name).id(egui::Id::new("git-branch-name")).desired_width(width-165.).char_limit(4096).hint_text("feature/my-change"));self.chrome.hits.insert("git-branch-name".into(),r.rect);
                if self.chrome.button(ui,"git-branch-create","Create branch",Icon::Add,Kind::Tonal,ready&&catalog.as_ref().is_some_and(|c|c.can_create())&&!self.git.branch_ui.name.is_empty(),150.,self.reduced_motion).on_hover_text_passive("Creates only a local ref at reviewed HEAD; no checkout, Save or commit. First commit required.").clicked(){self.git_branch_apply(Action::Create(self.git.branch_ui.name.clone()));}
            });
            let note=if self.git.error.is_empty(){self.git.note.as_str()}else{"Git stopped · edits/name/draft kept. Details or Refresh actual state."};
            let(r,_)=ui.allocate_exact_size(egui::vec2(width,18.),egui::Sense::hover());m::label_clipped(ui,r,note,11.,if self.git.error.is_empty(){m::MUTED}else{m::ERROR});
            if !self.git.error.is_empty(){let r=ui.selectable_label(self.git.details,"Error details");self.chrome.hits.insert("git-branch-details".into(),r.rect);if r.clicked(){self.git.details= !self.git.details;}
if self.git.details{egui::ScrollArea::vertical().id_salt("branch-errors").min_scrolled_height(0.).max_height(36.).show(ui,|ui|{ui.label(RichText::new(&self.git.error).small().color(m::ERROR));});}}
            if self.git.branch_ui.confirm&&let Some(n)=selected{
                let b=&catalog.as_ref().unwrap().items[n];let text=format!("Switch to {} · {}?\nGit may update working files. No Save, force, stash or commit.",b.name.to_string_lossy(),&b.oid[..8]);
                let(r,_)=ui.allocate_exact_size(egui::vec2(width,36.),egui::Sense::hover());m::label_clipped(ui,r,&text,12.,m::WARNING);ui.interact(r,ui.id().with("branch-review"),egui::Sense::hover()).on_hover_text_passive(text);
            }
            ui.horizontal(|ui|{
                let label=if self.git.branch_ui.confirm{"Switch branch"}else{"Review switch"};
                if self.chrome.button(ui,"git-branch-switch",label,Icon::Git,Kind::Filled,can_switch,160.,self.reduced_motion).on_hover_text_passive("Selection never switches. Review then explicitly confirm; Save/Undo all dirty tabs first.").clicked(){if self.git.branch_ui.confirm{self.git_branch_apply(Action::Switch(selected.unwrap()));}else{self.git.branch_ui.confirm=true;}}
                if self.chrome.button(ui,"git-branch-refresh","Refresh",Icon::Refresh,Kind::Tonal,!self.git.mutating,108.,self.reduced_motion).clicked(){self.git_refresh();}
                if self.chrome.button(ui,"git-branch-back","Changes",Icon::Close,Kind::Ghost,!self.git.mutating,116.,self.reduced_motion).clicked(){self.git_branches_back(ctx);}
                if self.git.busy{if self.reduced_motion{ui.label("Working…");}else{ui.spinner();}}
            });
        });
    }
}
