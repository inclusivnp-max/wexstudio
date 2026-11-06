pub mod adb;
pub mod cache;
pub mod editor;
pub mod formatting;
pub mod gradle;
mod k2_cache;
pub mod kotlin;
pub mod logcat;
pub mod lsp;
pub mod project;

pub mod language;

pub mod run;

pub mod native;

pub mod build;
pub mod manifest;

pub mod material;

pub mod editing;
pub mod editor_jobs;
pub mod editor_search;

pub mod dap;
pub mod debugger;

pub mod workspace_edit;

pub mod module_metadata;

pub mod version;

pub mod preview;

pub mod signing;

pub mod preview_catalog;

pub mod analysis;

pub mod module_scope;

mod resource_symbols;

pub mod model_watch;

pub mod compile_cache;

mod sdk_cache;

mod dex_cache;

pub mod resource_cache;

pub mod scaffold;

pub mod project_history;

pub mod runtime_paths;

pub mod file_ops;

pub mod document_jobs;

pub mod document_saves;

pub mod syntax_jobs;

pub mod document_retirement;

#[cfg(feature = "desktop")]
pub mod editor_mesh;

#[cfg(feature = "desktop")]
pub mod editor_layout;

#[cfg(feature = "desktop")]
pub mod terminal;

pub mod project_search;

pub mod file_icons;
pub mod material_symbols;

pub mod project_replace;

pub mod vcs;

pub mod java_format;

pub mod text_tracking;

pub mod breakpoint_store;
