//! Real per-file AAPT2 intermediate reuse; link/owned symbols always execute.
use crate::{
    build::{self, Step, Tools},
    compile_cache::{Cache, Input},
    run::RunEvent,
    sdk_cache::Guard,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};
pub(crate) const ABI: &str = "aapt-flat-files-v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub source: String,
    pub key: String,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub namespace: String,
    pub origin: String,
    pub files: usize,
    pub reused: usize,
    pub compiled: usize,
    pub uncached: usize,
    pub batches: usize,
    pub invalid: usize,
    pub detail: String,
    pub samples: Vec<Sample>,
}
#[derive(Default)]
pub(crate) struct Sources(pub BTreeMap<String, String>);
impl Sources {
    pub fn display(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (from, to) in &self.0 {
            text = text.replace(from, to);
        }
        text
    }
    pub fn extend(&mut self, other: Sources) {
        self.0.extend(other.0);
    }
}
pub(crate) struct Output {
    pub record: Record,
    pub steps: Vec<Step>,
    pub sources: Sources,
    pub checks: Vec<(PathBuf, String)>,
    pub tool_key: String,
}
fn os(p: impl AsRef<std::ffi::OsStr>) -> OsString {
    p.as_ref().to_owned()
}
pub(crate) fn body(path: &Path) -> Result<String> {
    crate::compile_cache::fingerprint(
        b"resource body",
        &[Input {
            label: "file".into(),
            path: path.into(),
            semantic_jar: false,
        }],
    )
}
pub(crate) fn flat_name(relative: &Path) -> Option<String> {
    let p = relative.components().collect::<Vec<_>>();
    if p.len() != 2
        || !p
            .iter()
            .all(|p| matches!(p, std::path::Component::Normal(_)))
    {
        return None;
    }
    let group = p[0].as_os_str().to_str()?;
    let name = p[1].as_os_str().to_str()?;
    if group.starts_with('.') || name.starts_with('.') || group.contains("flag(") {
        return None;
    }
    let name = if group.split('-').next() == Some("values") && name.ends_with(".xml") {
        format!("{}.arsc", name.strip_suffix(".xml")?)
    } else {
        name.into()
    };
    Some(format!("{group}_{name}.flat"))
}
struct Item {
    source: PathBuf,
    relative: PathBuf,
    origin: String,
    key: String,
    body: String,
    flat: String,
}
struct Pending {
    item: Item,
    input: PathBuf,
    out: tempfile::TempDir,
    cache: Option<Cache>,
    _guard: Option<Guard>,
}
fn key(tool: &str, owner: &str, origin: &str, relative: &Path, hash: &str) -> String {
    let mut h = blake3::Hasher::new();
    for x in [ABI, tool, owner, origin, &relative.to_string_lossy(), hash] {
        h.update(&(x.len() as u64).to_le_bytes());
        h.update(x.as_bytes());
    }
    h.finalize().to_hex().to_string()
}
fn input_snapshot(root: &Path, item: &Item) -> Result<PathBuf> {
    let cache = Cache::resource_input(root, &item.key)?;
    let verify = tempfile::tempdir()?;
    let origin_json = serde_json::to_vec(
        &serde_json::json!({"source":item.origin,"body":item.body,"relative":item.relative}),
    )?;
    let valid = cache.restore(verify.path()).ok().flatten().is_some()
        && body(&verify.path().join(&item.relative)).ok().as_deref() == Some(&item.body)
        && std::fs::read(verify.path().join("origin.json"))
            .ok()
            .as_deref()
            == Some(&origin_json);
    if !valid {
        let data = tempfile::tempdir()?;
        let path = data.path().join(&item.relative);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::copy(&item.source, &path)?;
        ensure!(
            body(&path)? == item.body,
            "resource changed while snapshotting"
        );
        std::fs::write(data.path().join("origin.json"), origin_json)?;
        cache.publish(data.path())?;
    }
    Ok(cache.payload_root().join(&item.relative))
}
pub(crate) async fn mapped(
    t: &Tools,
    root: &Path,
    name: &str,
    program: &Path,
    args: Vec<OsString>,
    sources: &Sources,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Step> {
    let shown = |e| match e {
        RunEvent::BuildLine(s) => event(RunEvent::BuildLine(sources.display(&s))),
        other => event(other),
    };
    build::command(t, root, name, program, args, &shown)
        .await
        .map_err(|e| anyhow::anyhow!(sources.display(&format!("{e:#}"))))
}
fn zip_outputs(zip: &Path, outputs: &[(String, PathBuf)]) -> Result<()> {
    let mut z = zip::ZipWriter::new(std::fs::File::create(zip)?);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, path) in outputs {
        z.start_file(name, options)?;
        std::io::copy(&mut std::fs::File::open(path)?, &mut z)?;
    }
    z.finish()?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn compile(
    t: &Tools,
    root: &Path,
    cache_root: &Path,
    namespace: &str,
    origin: &str,
    dir: &Path,
    zip: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Output> {
    let files = build::files(dir, &[])?
        .into_iter()
        .filter(|p| {
            !p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with('.')
        })
        .collect::<Vec<_>>();
    let mut record = Record {
        namespace: namespace.into(),
        origin: origin.into(),
        files: files.len(),
        reused: 0,
        compiled: 0,
        uncached: 0,
        batches: 0,
        invalid: 0,
        detail: String::new(),
        samples: vec![],
    };
    let tool = build::resource_tool_key(t);
    let program = build::resource_program(t);
    let shapes = files
        .iter()
        .map(|p| flat_name(p.strip_prefix(dir).unwrap()))
        .collect::<Option<Vec<_>>>();
    if shapes.is_none() || tool.is_err() {
        record.compiled = files.len();
        record.uncached = files.len();
        record.batches = 1;
        record.detail = "Unsupported resource shape/tool input; real directory compilation".into();
        let step = build::command(
            t,
            root,
            "AAPT2 compile",
            &program,
            vec![os("compile"), os("--dir"), os(dir), os("-o"), os(zip)],
            event,
        )
        .await?;
        return Ok(Output {
            record,
            steps: vec![step],
            sources: Sources::default(),
            checks: vec![],
            tool_key: String::new(),
        });
    }
    let tool = tool?;
    let shapes = shapes.unwrap();
    let mut unique = BTreeSet::new();
    ensure!(
        shapes.iter().all(|p| unique.insert(p)),
        "colliding compiled resource filenames"
    );
    let storage = tempfile::tempdir_in(zip.parent().context("resource work parent")?)?;
    let mut steps = vec![];
    let mut sources = Sources::default();
    let mut checks = vec![];
    let mut ordered = vec![];
    let mut pending = vec![];
    for (i, (source, flat)) in files.iter().zip(shapes).enumerate() {
        let relative = source.strip_prefix(dir)?.to_owned();
        let origin_file = format!("{origin}/{}", relative.to_string_lossy());
        let hash = body(source)?;
        let key = key(&tool, namespace, &origin_file, &relative, &hash);
        let item = Item {
            source: source.clone(),
            relative,
            origin: origin_file,
            key: key.clone(),
            body: hash.clone(),
            flat: flat.clone(),
        };
        checks.push((source.clone(), hash));
        let sample_origin = item.origin.clone();
        let out = tempfile::tempdir_in(storage.path())?;
        let mut cache = None;
        let mut guard = None;
        let mut state = "miss";
        let input = match Cache::resource_file(cache_root, &key)
            .and_then(|c| Ok((c, Guard::resource(cache_root, &key)?)))
        {
            Ok((c, Some(g))) => match input_snapshot(cache_root, &item) {
                Ok(input) => {
                    cache = Some(c);
                    guard = Some(g);
                    input
                }
                Err(e) => {
                    record.uncached += 1;
                    record.detail = format!("Snapshot unavailable; compiling independently: {e:#}");
                    state = "uncached";
                    item.source.clone()
                }
            },
            Ok((_, None)) => {
                record.uncached += 1;
                record.detail = "Resource entry busy; independent compile".into();
                state = "uncached";
                item.source.clone()
            }
            Err(e) => {
                record.uncached += 1;
                record.detail = format!("Resource cache unavailable: {e:#}");
                state = "uncached";
                item.source.clone()
            }
        };
        let input = if cache.is_none() {
            let path = storage
                .path()
                .join(format!("input-{i}"))
                .join(&item.relative);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::copy(&item.source, &path)?;
            ensure!(
                body(&path)? == item.body,
                "resource changed while copying independent input"
            );
            path
        } else {
            input
        };
        sources
            .0
            .insert(input.to_string_lossy().into_owned(), item.origin.clone());
        let work_flat = storage.path().join(format!("{i}.flat"));
        let reused = if let Some(c) = &cache {
            match c.restore(out.path()) {
                Ok(Some(1)) if out.path().join(&flat).is_file() => true,
                Ok(None) => false,
                other => {
                    record.invalid += 1;
                    record.detail = format!("Invalid resource entry; recompiling: {other:?}");
                    std::fs::remove_dir_all(out.path())?;
                    std::fs::create_dir(out.path())?;
                    false
                }
            }
        } else {
            false
        };
        if reused {
            std::fs::copy(out.path().join(&flat), &work_flat)?;
            record.reused += 1;
            state = "hit";
        } else {
            pending.push(Pending {
                item,
                input,
                out,
                cache,
                _guard: guard,
            });
        }
        if record.samples.len() < 8 {
            record.samples.push(Sample {
                source: sample_origin,
                key,
                state: state.into(),
            });
        }
        ordered.push((flat, work_flat));
        // Bound both argv and simultaneously held miss locks; original tree order
        // is restored in the final ZIP, independent of SDK input-path sorting.
        if pending.len() == 128 {
            flush(
                t,
                root,
                &program,
                &tool,
                &mut pending,
                &ordered,
                &sources,
                &mut record,
                &mut steps,
                event,
            )
            .await?;
        }
    }
    if !pending.is_empty() {
        flush(
            t,
            root,
            &program,
            &tool,
            &mut pending,
            &ordered,
            &sources,
            &mut record,
            &mut steps,
            event,
        )
        .await?;
    }
    for (path, hash) in &checks {
        ensure!(
            body(path)? == *hash,
            "resource inputs changed during compilation"
        );
    }
    ensure!(
        build::resource_tool_key(t)? == tool,
        "AAPT2 inputs changed during compilation"
    );
    zip_outputs(zip, &ordered)?;
    record.detail = record.detail.chars().take(1024).collect();
    event(RunEvent::Phase(format!(
        "RESOURCE_FILES · {namespace} · {} / {} reused · {} compiled · {} batches",
        record.reused, record.files, record.compiled, record.batches
    )));
    Ok(Output {
        record,
        steps,
        sources,
        checks,
        tool_key: tool,
    })
}
#[allow(clippy::too_many_arguments)]
async fn flush(
    t: &Tools,
    root: &Path,
    program: &Path,
    tool: &str,
    pending: &mut Vec<Pending>,
    ordered: &[(String, PathBuf)],
    sources: &Sources,
    record: &mut Record,
    steps: &mut Vec<Step>,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<()> {
    let batch = tempfile::tempdir_in(ordered[0].1.parent().unwrap())?;
    let zip = batch.path().join("compiled.zip");
    let mut args = vec![os("compile"), os("-o"), os(&zip)];
    args.extend(pending.iter().map(|p| os(&p.input)));
    steps.push(mapped(t, root, "AAPT2 compile", program, args, sources, event).await?);
    record.batches += 1;
    ensure!(
        build::resource_tool_key(t)? == tool,
        "AAPT2 changed during resource batch"
    );
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip)?)?;
    ensure!(
        archive.len() == pending.len(),
        "unexpected AAPT2 intermediate count"
    );
    let expected = pending
        .iter()
        .map(|p| p.item.flat.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i)?;
        ensure!(
            expected.contains(f.name()) && seen.insert(f.name().to_owned()),
            "unexpected/duplicate AAPT2 intermediate"
        );
    }
    for p in pending.drain(..) {
        ensure!(
            body(&p.item.source)? == p.item.body,
            "resource changed during AAPT2 batch"
        );
        let mut f = archive.by_name(&p.item.flat)?;
        ensure!(f.size() <= 512 * 1024 * 1024, "AAPT2 flat size limit");
        let path = p.out.path().join(&p.item.flat);
        let mut out = std::fs::File::create(&path)?;
        std::io::copy(&mut f, &mut out)?;
        out.flush()?;
        if let Some(cache) = &p.cache
            && let Err(e) = cache.publish(p.out.path())
        {
            record.uncached += 1;
            record.detail = format!("Compiled; resource store skipped: {e:#}");
        }
        let target = ordered
            .iter()
            .find(|(n, _)| n == &p.item.flat)
            .context("resource order mapping missing")?;
        std::fs::copy(&path, &target.1)?;
        record.compiled += 1;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_flat_names_preserve_qualifiers_raw_nine_patch_and_values_tables() {
        for (a, b) in [
            ("values/strings.xml", "values_strings.arsc.flat"),
            (
                "values-en-rUS/strings.xml",
                "values-en-rUS_strings.arsc.flat",
            ),
            ("drawable-hdpi/icon.9.png", "drawable-hdpi_icon.9.png.flat"),
            ("raw/blob.bin", "raw_blob.bin.flat"),
            ("layout/scene.xml", "layout_scene.xml.flat"),
        ] {
            assert_eq!(flat_name(Path::new(a)).as_deref(), Some(b));
        }
        assert!(flat_name(Path::new("flag(foo)/values/file.xml")).is_none());
    }
    #[test]
    fn keys_bind_origin_owner_contents_tool_and_relative_path() {
        let a = key(
            "tool",
            "owner",
            "/res/a.xml",
            Path::new("values/a.xml"),
            "bytes",
        );
        for (t, o, s, p, b) in [
            ("new", "owner", "/res/a.xml", "values/a.xml", "bytes"),
            ("tool", "different", "/res/a.xml", "values/a.xml", "bytes"),
            ("tool", "owner", "/res/b.xml", "values/a.xml", "bytes"),
            ("tool", "owner", "/res/a.xml", "values-en/a.xml", "bytes"),
            ("tool", "owner", "/res/a.xml", "values/a.xml", "new"),
        ] {
            assert_ne!(a, key(t, o, s, Path::new(p), b));
        }
    }
    #[test]
    fn diagnostic_display_maps_stable_inputs_without_rewriting_arguments() {
        let mut m = Sources::default();
        m.0.insert(
            "/engine/key/source/values/a.xml".into(),
            "/project/res/values/a.xml".into(),
        );
        assert_eq!(
            m.display("/engine/key/source/values/a.xml:7: error"),
            "/project/res/values/a.xml:7: error"
        );
    }
}
