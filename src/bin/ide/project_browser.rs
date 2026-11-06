//! Background folder navigation with input-scoped receivers and last-good/error state.
use super::*;
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    project_history::{self, Listing},
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Purpose {
    Parent,
    Existing,
}
pub(super) struct Browser {
    pub purpose: Purpose,
    pub address: String,
    requested_address: String,
    requested: PathBuf,
    pub hidden: bool,
    pub filter: String,
    pub listing: Option<Listing>,
    pub error: Option<String>,
    pub busy: bool,
    rx: Option<mpsc::Receiver<Result<Listing, String>>>,
}
impl Browser {
    pub fn new(purpose: Purpose, path: PathBuf) -> Self {
        let mut s = Self {
            purpose,
            address: path.to_string_lossy().into_owned(),
            requested_address: String::new(),
            requested: path.clone(),
            hidden: false,
            filter: String::new(),
            listing: None,
            error: None,
            busy: false,
            rx: None,
        };
        s.load(path);
        s
    }
    pub fn load(&mut self, path: PathBuf) {
        self.requested = path.clone();
        self.requested_address = self.address.clone();
        self.busy = true;
        self.error = None;
        let hidden = self.hidden;
        let (tx, rx) = mpsc::sync_channel(1);
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(project_history::folders(&path, hidden).map_err(|e| format!("{e:#}")));
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(r) => {
                    self.rx = None;
                    self.busy = false;
                    match r {
                        Ok(l) => {
                            if self.address == self.requested_address {
                                self.address = l.root.to_string_lossy().into_owned();
                            }
                            self.requested = l.root.clone();
                            self.listing = Some(l);
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.busy = false;
                    self.error = Some("Folder worker stopped; retry".into());
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50))
                }
            }
        }
    }
}
impl Workbench {
    pub(super) fn browser_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut b) = self.projects.browser.take() else {
            return;
        };
        b.poll(ctx);
        let mut navigate = None;
        let mut choose = None;
        let mut cancel = false;
        let reduced = self.reduced_motion;
        let modal=egui::Modal::new(egui::Id::new("project-folder-browser")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-100.).clamp(320.,600.));ui.heading(match b.purpose{Purpose::Parent=>"Choose a parent folder",Purpose::Existing=>"Choose a project folder"});
            ui.label(RichText::new("Folders only · nothing is created or opened here").color(m::MUTED));ui.add_space(12.);
            ui.horizontal(|ui|{let r=ui.add(egui::TextEdit::singleline(&mut b.address).desired_width(ui.available_width()-86.).id_salt("browser-address"));self.chrome.hits.insert("browser-address".into(),r.rect);let enter=r.lost_focus()&&ui.input(|i|i.key_pressed(Key::Enter));if self.chrome.button(ui,"browser-go","Go",Icon::Folder,Kind::Tonal,true,70.,reduced).clicked()||enter{navigate=Some(PathBuf::from(&b.address));}});
            ui.horizontal(|ui|{
                let parent=b.listing.as_ref().and_then(|l|l.parent.clone()).or_else(||b.requested.parent().map(std::path::Path::to_owned));
                if self.chrome.button(ui,"browser-up","Up",Icon::Folder,Kind::Tonal,parent.is_some(),90.,reduced).clicked(){navigate=parent;}
                if self.chrome.button(ui,"browser-home","Home",Icon::Folder,Kind::Ghost,true,100.,reduced).clicked(){navigate=Some(PathBuf::from(std::env::var_os("HOME").unwrap_or_default()));}
                if self.chrome.button(ui,"browser-refresh","Refresh",Icon::Refresh,Kind::Ghost,true,110.,reduced).clicked(){navigate=Some(b.requested.clone());}
                let r=ui.checkbox(&mut b.hidden,"Hidden");self.chrome.hits.insert("browser-hidden".into(),r.rect);if r.changed(){navigate=Some(b.requested.clone());}
            });
            ui.add_space(6.);let r=ui.add(egui::TextEdit::singleline(&mut b.filter).hint_text("Filter folders").desired_width(f32::INFINITY).id_salt("browser-filter"));self.chrome.hits.insert("browser-filter".into(),r.rect);
            if b.busy{ui.label(RichText::new("Loading folders…").color(m::MUTED));}
                if let Some(e)=&b.error{ui.label(RichText::new(e).color(m::ERROR));}
            if let Some(l)=&b.listing {
                ui.label(RichText::new(l.root.to_string_lossy()).small().color(m::MUTED));
                let filter=b.filter.to_lowercase();let filtered=l.folders.iter().filter(|f|f.name.to_lowercase().contains(&filter)).collect::<Vec<_>>();
                if filtered.is_empty(){ui.label(RichText::new("No matching folders").color(m::MUTED));}
                egui::ScrollArea::vertical().id_salt("browser-folders").max_height((ctx.content_rect().height()-420.).clamp(120.,420.)).show_rows(ui,40.,filtered.len(),|ui,range|{
                    for index in range {let f=filtered[index];ui.horizontal(|ui|{let key=format!("browser-folder:{}",f.path.display());let label=format!("{}{}",f.name,if f.symlink{" ↗"}else{""});let r=self.chrome.button(ui,&key,&label,Icon::Folder,Kind::Ghost,!b.busy&&b.error.is_none(),(ui.available_width()-110.).max(160.),reduced);if r.clicked(){navigate=Some(f.path.clone());}r.on_hover_text_passive(f.path.to_string_lossy());if f.wex_config{m::badge(ui,"wex.json",m::PRIMARY);}});}
                });
                if l.truncated{ui.label(RichText::new("Partial listing: folder/scan limit reached. Navigate with the address field.").color(m::WARNING));}
                if l.skipped_errors>0{ui.label(format!("{} inaccessible entries skipped",l.skipped_errors));}
            }
            ui.add_space(12.);ui.horizontal(|ui|{
                if self.chrome.button(ui,"browser-use","Use this folder",Icon::Check,Kind::Filled,!b.busy&&b.error.is_none()&&b.listing.is_some(),190.,reduced).clicked(){choose=b.listing.as_ref().map(|l|l.root.clone());}
                if self.chrome.button(ui,"browser-cancel","Cancel",Icon::Close,Kind::Tonal,true,110.,reduced).clicked(){cancel=true;}
            });
        });
        if let Some(p) = choose {
            match b.purpose {
                Purpose::Parent => self.projects.parent = p.to_string_lossy().into_owned(),
                Purpose::Existing => self.projects.existing = p.to_string_lossy().into_owned(),
            };
            self.projects.validation_key.clear();
            return;
        }
        let escape = modal.is_top_modal
            && !modal.any_popup_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if cancel || escape {
            return;
        }
        if let Some(p) = navigate {
            b.load(p);
        }
        self.projects.browser = Some(b);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn superseded_receiver_cannot_publish_stale_listing() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("A");
        let b = t.path().join("B");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut browser = Browser::new(Purpose::Parent, a);
        browser.load(b.clone());
        let ctx = egui::Context::default();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while browser.busy {
            assert!(std::time::Instant::now() < end);
            browser.poll(&ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(browser.listing.unwrap().root, b);
    }
    #[test]
    fn failed_navigation_retains_last_good_but_disables_selection() {
        let t = tempfile::tempdir().unwrap();
        let mut b = Browser::new(Purpose::Existing, t.path().to_owned());
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while b.busy {
                assert!(std::time::Instant::now() < end);
                b.poll(&ctx);
                std::thread::sleep(Duration::from_millis(2));
            }
            if b.error.is_none() {
                b.load(t.path().join("missing"));
            }
        }
        assert!(b.error.is_some());
        assert_eq!(b.listing.unwrap().root, t.path());
    }
}
