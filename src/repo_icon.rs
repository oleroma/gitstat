//! A repo's own icon, `<repo>/.gitstat/icon.svg`, drawn in its status
//! colour for the `repo-svg` folder icon mode.
//!
//! The file comes from the repo, so it is untrusted input: it is read only
//! up to [`MAX_BYTES`], never through a link, and nothing it refers to is
//! loaded (no files, no network, no embedded images, no fonts). What it
//! would expand to is estimated before usvg builds it, filters are refused,
//! and it is drawn on a thread of its own, so a panic there is an error like
//! any other. Any failure means the folder shows the status icon instead.

use crate::icon::{self, FOLDER_SIZES, Tone};
use resvg::usvg::roxmltree;
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Where a repo keeps its icon, relative to the repo folder.
pub const DIR: &str = ".gitstat";
pub const FILE: &str = "icon.svg";
/// Larger files are refused unread.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// Icons are a handful of shapes. Far more than that is refused before
/// drawing, which could take long: every group with an opacity, a mask or
/// a filter is a layer the size of the icon.
const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 64;
/// XML nodes (elements, text, comments) the file may have.
const MAX_XML_NODES: u32 = 100_000;
/// What usvg may build from the file, every copy counted (see
/// [`check_expansion`]): elements, and bytes of path data it parses.
const MAX_EXPANDED_NODES: u64 = 10 * MAX_NODES as u64;
const MAX_EXPANDED_PATH_BYTES: u64 = 4 * MAX_BYTES;
/// Elements and the references between them, nested.
const MAX_EXPANDED_DEPTH: usize = 4 * MAX_DEPTH;
/// Style rules with a `url(…)` times elements: the selector matches
/// [`check_expansion`] makes.
const MAX_CSS_MATCHES: u64 = 10_000_000;
const RENDER_STACK: usize = 64 * 1024 * 1024;
const RENDER_TIMEOUT: Duration = Duration::from_secs(10);
/// Drawings given up on that may still be going, across all repos, before
/// no more are started.
const MAX_LATE: usize = 2;
/// Bump when the drawing changes: it is part of every repo icon's file name,
/// since Explorer caches icons by path, and of its fingerprint, so every
/// folder is drawn again once.
pub(crate) const RENDER_VERSION: u8 = 1;
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Why a repo icon can't be shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub message: String,
    /// Not the file's fault: it couldn't be read, or drawing it took too
    /// long (maybe the machine was busy) or is still going. Worth another
    /// try, where a file that can't be drawn isn't until it changes.
    pub transient: bool,
}

impl Error {
    fn file(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn svg_path(repo: &Path) -> PathBuf {
    repo.join(DIR).join(FILE)
}

/// The repo's icon.svg, `None` when it has none. Refuses a file over
/// [`MAX_BYTES`], and a file or `.gitstat` folder that is a link (or a
/// junction), which could point anywhere, such as a network share.
pub fn load(repo: &Path) -> Result<Option<Vec<u8>>, Error> {
    let dir = repo.join(DIR);
    let file = svg_path(repo);
    let io = |e: std::io::Error| Error::transient(format!("{DIR}/{FILE}: {e}"));
    for path in [&dir, &file] {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::file(format!("{DIR}/{FILE}: links aren't followed")));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        }
    }
    let too_big = || Error::file(format!("{DIR}/{FILE} is over {} KiB", MAX_BYTES / 1024));
    let meta = std::fs::metadata(&file).map_err(io)?;
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_BYTES {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(io)?;
    // It may have grown since.
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_big());
    }
    Ok(Some(bytes))
}

/// What the folder-icon plan compares to see whether a repo's icon
/// changed: [`fingerprint_of`] the file, or why it can't be used. `None`
/// when the repo has no icon.svg.
pub fn fingerprint(repo: &Path) -> Option<String> {
    match load(repo) {
        Ok(bytes) => bytes.map(|b| fingerprint_of(&b)),
        Err(e) => Some(format!("unusable: {e}")),
    }
}

/// A hash of the file and the [`RENDER_VERSION`] that draws it.
pub fn fingerprint_of(svg: &[u8]) -> String {
    fingerprint_at(RENDER_VERSION, svg)
}

pub(crate) fn fingerprint_at(version: u8, svg: &[u8]) -> String {
    format!("v{version}-{:016x}", fnv(&[svg]))
}

/// The .ico's file name: one per repo, tone and icon, so that Explorer,
/// which caches icons by path, sees a new path whenever the picture changes.
pub fn file_name(repo: &Path, tone: Tone, svg: &[u8]) -> String {
    let repo_key = crate::folder_icons::key(repo);
    format!(
        "repo-{:016x}-{}-{:016x}.ico",
        fnv(&[repo_key.as_bytes()]),
        tone.name(),
        fnv(&[&[RENDER_VERSION], svg])
    )
}

/// Whether `name` is a file `file_name` makes, or its temp file.
pub fn is_repo_icon_file(name: &str) -> bool {
    name.starts_with("repo-") && (name.ends_with(".ico") || name.ends_with(".ico.tmp"))
}

/// FNV-1a: stable across Rust versions and platforms, unlike `DefaultHasher`,
/// since the names outlive the process. Not a defence against anything.
fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &b in *part {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// Draws repo icons, each on a thread of its own with a large stack: usvg
/// recurses once per level of nesting (to 1024 levels, `<use>` chains
/// included), and a stack overflow would end the process where a panic
/// only ends that thread.
///
/// A drawing that takes too long is given up on and left to finish on its
/// thread. Until it has, its repo gets no other drawing (and past
/// [`MAX_LATE`] such drawings, no repo does), so a file that keeps a core
/// busy doesn't get another one each time its repo changes colour. What
/// it finally draws is kept for the next try at the same file and tone.
pub struct Renderer {
    late: Arc<Mutex<HashMap<String, Late>>>,
    timeout: Duration,
}

/// Straight RGBA pictures, by size in px.
pub type Images = Vec<(u32, Vec<u8>)>;

/// A drawing given up on, by repo.
struct Late {
    /// Its file and tone.
    key: u64,
    /// `None` while it is still going.
    result: Option<Result<Images, String>>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::with_timeout(RENDER_TIMEOUT)
    }
}

impl Renderer {
    fn with_timeout(timeout: Duration) -> Self {
        Self {
            late: Arc::default(),
            timeout,
        }
    }

    /// The complete .ico for `svg` in `tone`, every size in
    /// [`FOLDER_SIZES`]. `repo` names whose drawing it is, for the limit
    /// above.
    ///
    /// Every fill or stroke that is `currentColor` takes the tone's colour
    /// and the other colours stay. An SVG that never mentions
    /// `currentColor` is drawn as a silhouette in that colour instead: its
    /// shape and transparency, none of its colours.
    pub fn render(&self, repo: &str, svg: &[u8], tone: Tone) -> Result<Vec<u8>, Error> {
        self.render_rgba(repo, svg, tone, &FOLDER_SIZES)
            .map(|images| icon::encode_ico(&images))
    }

    /// Like [`Renderer::render`], as straight RGBA at each of `sizes` px,
    /// for the app's own windows.
    pub fn render_rgba(
        &self,
        repo: &str,
        svg: &[u8],
        tone: Tone,
        sizes: &[u32],
    ) -> Result<Images, Error> {
        let key = late_key(svg, tone, sizes);
        {
            let mut late = lock(&self.late);
            match late.remove(repo) {
                Some(Late {
                    key: k,
                    result: Some(result),
                }) if k == key => return result.map_err(Error::file),
                Some(going @ Late { result: None, .. }) => {
                    late.insert(repo.to_string(), going);
                    return Err(Error::transient(format!(
                        "{DIR}/{FILE} is still being drawn"
                    )));
                }
                // A result for another file or tone: of no more use.
                _ => {}
            }
            if late.values().filter(|l| l.result.is_none()).count() >= MAX_LATE {
                return Err(Error::transient(format!(
                    "{DIR}/{FILE} must wait: other repos' icons are still being drawn"
                )));
            }
        }
        let (tx, rx) = mpsc::channel();
        let svg = svg.to_vec();
        let slots = Arc::clone(&self.late);
        let slot = repo.to_string();
        let sizes = sizes.to_vec();
        std::thread::Builder::new()
            .name("repo-icon".into())
            .stack_size(RENDER_STACK)
            .spawn(move || {
                let result = std::panic::catch_unwind(|| render_all(&svg, tone.color(), &sizes))
                    .unwrap_or_else(|_| Err(format!("{DIR}/{FILE} couldn't be drawn")));
                // Under the lock, so the caller either receives it or has
                // left the slot for it.
                let mut late = lock(&slots);
                if let Err(mpsc::SendError(result)) = tx.send(result)
                    && let Some(going) = late.get_mut(&slot)
                    && going.key == key
                    && going.result.is_none()
                {
                    going.result = Some(result);
                }
            })
            .map_err(|e| Error::transient(format!("can't draw {DIR}/{FILE}: {e}")))?;
        match rx.recv_timeout(self.timeout) {
            Ok(result) => result.map_err(Error::file),
            Err(RecvTimeoutError::Timeout) => {
                let mut late = lock(&self.late);
                if let Ok(result) = rx.try_recv() {
                    return result.map_err(Error::file);
                }
                late.insert(repo.to_string(), Late { key, result: None });
                drop(rx);
                drop(late);
                Err(Error::transient(format!(
                    "{DIR}/{FILE} takes too long to draw"
                )))
            }
            // It panicked outside `catch_unwind`, so nothing was sent.
            Err(RecvTimeoutError::Disconnected) => {
                Err(Error::file(format!("{DIR}/{FILE} couldn't be drawn")))
            }
        }
    }
}

/// What a drawing given up on drew: its file, tone and sizes.
fn late_key(svg: &[u8], tone: Tone, sizes: &[u32]) -> u64 {
    let sizes: Vec<u8> = sizes.iter().flat_map(|s| s.to_le_bytes()).collect();
    fnv(&[svg, tone.name().as_bytes(), &sizes])
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn render_all(svg: &[u8], color: [u8; 3], sizes: &[u32]) -> Result<Images, String> {
    if svg.len() as u64 > MAX_BYTES {
        return Err(format!("{DIR}/{FILE} is over {} KiB", MAX_BYTES / 1024));
    }
    let unusable = |e: &dyn std::fmt::Display| format!("{DIR}/{FILE} isn't usable SVG: {e}");
    let text = std::str::from_utf8(svg).map_err(|_| unusable(&"not UTF-8"))?;
    let (text, silhouette) = match spell_current_color(text) {
        Some(text) => (text, false),
        None => (text.to_string(), true),
    };
    let text = text.as_str();
    if markup_depth(text)? > MAX_DEPTH {
        return Err(format!("{DIR}/{FILE} nests too deep"));
    }
    let xml_options = roxmltree::ParsingOptions {
        // A `<!DOCTYPE>` line, which exported drawings often have.
        // `markup_depth` refused any entities.
        allow_dtd: true,
        nodes_limit: MAX_XML_NODES,
        ..Default::default()
    };
    let xml =
        roxmltree::Document::parse_with_options(text, xml_options).map_err(|e| unusable(&e))?;
    check_expansion(&xml)?;
    let tree = usvg::Tree::from_xmltree(&xml, &options(color)).map_err(|e| unusable(&e))?;
    check_complexity(&tree)?;
    let images = sizes
        .iter()
        .map(|&size| {
            let rgba = render_at(&tree, size)?;
            Ok((size, if silhouette { tint(rgba, color) } else { rgba }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let drew = images
        .last()
        .is_some_and(|(_, rgba)| rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0));
    if !drew {
        return Err(format!("{DIR}/{FILE} draws nothing"));
    }
    Ok(images)
}

/// How deep the elements nest, counted from the markup itself: the XML
/// parser recurses once per level, so a deep enough file would overflow
/// any stack before a parsed depth could be checked. Refuses a DTD with
/// an internal subset, whose entities could add nesting this doesn't see.
///
/// It may count too high for a malformed file, which is then refused
/// either way, but never too low for one the parser would accept: it
/// finds every start tag the parser would, and a close tag it counts
/// down for without the parser closing an element is a parse error there.
fn markup_depth(text: &str) -> Result<usize, String> {
    let b = text.as_bytes();
    let find = |from: usize, needle: &[u8]| {
        b.get(from..)
            .and_then(|rest| rest.windows(needle.len()).position(|w| w == needle))
            .map_or(b.len(), |at| from + at + needle.len())
    };
    // Past the `>` that ends a tag or declaration starting at `from`,
    // skipping quoted attribute values; whether it was `/>`; and whether
    // a `[` came first, outside quotes.
    let tag_end = |from: usize| {
        let mut quote = None;
        for (i, &c) in b.iter().enumerate().skip(from) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(c),
                (None, b'[') => return (i, false, true),
                (None, b'>') => return (i + 1, i > from && b[i - 1] == b'/', false),
                (None, _) => {}
            }
        }
        (b.len(), false, false)
    };
    let (mut i, mut depth, mut max) = (0, 0usize, 0usize);
    while let Some(at) = b
        .get(i..)
        .and_then(|rest| rest.iter().position(|&c| c == b'<'))
    {
        let at = i + at;
        let rest = &b[at..];
        i = if rest.starts_with(b"<!--") {
            find(at + 4, b"-->")
        } else if rest.starts_with(b"<![CDATA[") {
            find(at + 9, b"]]>")
        } else if rest.starts_with(b"<?") {
            find(at + 2, b"?>")
        } else if rest.starts_with(b"<!") {
            let (end, _, subset) = tag_end(at + 2);
            if subset {
                return Err(format!(
                    "{DIR}/{FILE} has DTD entities, which aren't supported"
                ));
            }
            end
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            at + 2
        } else {
            let (end, empty, _) = tag_end(at + 1);
            if !empty {
                depth += 1;
                max = max.max(depth);
            }
            end
        };
    }
    Ok(max)
}

/// Parsing options that load nothing from outside the file, and make
/// `currentColor` mean the tone everywhere: an `!important` rule in the
/// injected style sheet beats the file's own `color` attributes, style
/// sheets and `style` attributes, `!important` or not.
fn options(color: [u8; 3]) -> usvg::Options<'static> {
    let [r, g, b] = color;
    usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        style_sheet: Some(format!("* {{ color: #{r:02x}{g:02x}{b:02x} !important; }}")),
        ..usvg::Options::default()
    }
}

/// What usvg would build from the file, every copy counted, refused over
/// [`MAX_EXPANDED_NODES`] elements or [`MAX_EXPANDED_PATH_BYTES`] of path
/// data before usvg builds any of it: a file of a few KiB can expand to
/// gigabytes, and running out of memory ends the process, not a thread.
///
/// usvg copies what a `<use>` points at, every time. Each element that
/// takes a pattern, gradient, clip path or mask (from an attribute, its
/// `style` or a style sheet) gets its own copy, and so does every element
/// inside it, which inherits paint; a marker is copied for every vertex.
/// The estimate counts all of that and refuses references that loop,
/// which usvg would follow until its stack overflows.
fn check_expansion(xml: &roxmltree::Document) -> Result<(), String> {
    let mut ids: HashMap<&str, Vec<roxmltree::Node>> = HashMap::new();
    let mut sheet = simplecss::StyleSheet::new();
    let mut elements = 0u64;
    for node in xml.descendants().filter(is_svg_element) {
        elements += 1;
        if let Some(id) = node.attribute("id") {
            ids.entry(id).or_default().push(node);
        }
        // The style sheets usvg reads.
        if node.tag_name().name() == "style"
            && let Some(text) = node.text()
        {
            sheet.parse_more(text);
        }
    }
    let rules: Vec<_> = sheet
        .rules
        .into_iter()
        .filter_map(|rule| {
            let refs: Vec<&str> = rule
                .declarations
                .iter()
                .flat_map(|d| func_iris(d.value))
                .collect();
            (!refs.is_empty()).then_some((rule.selector, refs))
        })
        .collect();
    if (rules.len() as u64).saturating_mul(elements) > MAX_CSS_MATCHES {
        return Err(format!(
            "{DIR}/{FILE} has too many style rules for its size"
        ));
    }
    let mut scan = Expansion {
        ids,
        rules,
        seen: HashMap::new(),
        id_costs: HashMap::new(),
    };
    scan.cost(xml.root_element(), 1).map(|_| ())
}

fn is_svg_element(node: &roxmltree::Node) -> bool {
    node.is_element() && matches!(node.tag_name().namespace(), None | Some(SVG_NS))
}

/// What one element expands to, with everything it contains and refers to.
#[derive(Clone, Copy, Default)]
struct Cost {
    nodes: u64,
    path_bytes: u64,
    /// Elements and vertices that take on paint and markers set here:
    /// its own and its children's, and what a `<use>` copies in.
    elements: u64,
    vertices: u64,
    /// Levels of elements and references below it.
    depth: usize,
}

impl Cost {
    /// `inner` nested inside.
    fn contain(&mut self, inner: Cost) {
        self.nodes = self.nodes.saturating_add(inner.nodes);
        self.path_bytes = self.path_bytes.saturating_add(inner.path_bytes);
        self.elements = self.elements.saturating_add(inner.elements);
        self.vertices = self.vertices.saturating_add(inner.vertices);
        self.depth = self.depth.max(inner.depth + 1);
    }

    /// `times` copies of `target`, which takes nothing on from here.
    fn refer(&mut self, target: Cost, times: u64) {
        let nodes = target.nodes.saturating_mul(times);
        self.nodes = self.nodes.saturating_add(nodes);
        let bytes = target.path_bytes.saturating_mul(times);
        self.path_bytes = self.path_bytes.saturating_add(bytes);
        self.depth = self.depth.max(target.depth + 1);
    }
}

enum Seen {
    Visiting,
    Done(Cost),
}

struct Expansion<'a, 'input> {
    /// SVG elements by `id`, all of them for an id used twice.
    ids: HashMap<&'a str, Vec<roxmltree::Node<'a, 'input>>>,
    /// The style rules that refer to something, and the ids they name.
    rules: Vec<(simplecss::Selector<'a>, Vec<&'a str>)>,
    seen: HashMap<roxmltree::NodeId, Seen>,
    /// By id: what the elements it names cost, markers apart.
    id_costs: HashMap<&'a str, (Cost, Cost)>,
}

impl<'a, 'input> Expansion<'a, 'input> {
    fn cost(&mut self, node: roxmltree::Node<'a, 'input>, depth: usize) -> Result<Cost, String> {
        let deep = || format!("{DIR}/{FILE} nests too deep");
        if depth > MAX_EXPANDED_DEPTH {
            return Err(deep());
        }
        match self.seen.get(&node.id()) {
            Some(Seen::Done(cost)) if depth + cost.depth > MAX_EXPANDED_DEPTH => return Err(deep()),
            Some(Seen::Done(cost)) => return Ok(*cost),
            Some(Seen::Visiting) => return Err(format!("{DIR}/{FILE} refers to itself")),
            None => {}
        }
        self.seen.insert(node.id(), Seen::Visiting);
        let tag = node.tag_name().name();
        let data: u64 = ["d", "points"]
            .iter()
            .filter_map(|a| node.attribute(*a))
            .map(|v| v.len() as u64)
            .sum();
        let shape = matches!(
            tag,
            "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon"
        );
        let mut cost = Cost {
            nodes: 1,
            path_bytes: data,
            elements: 1,
            // A vertex takes at least a byte (`z`), and the basic shapes
            // have a few without any path data.
            vertices: if shape { data + 8 } else { 0 },
            depth: 0,
        };
        let mut inner: Vec<_> = node.children().filter(is_svg_element).collect();
        if tag == "use"
            && let Some(id) = node.attribute("href").and_then(local_iri)
        {
            inner.extend(self.ids.get(id).into_iter().flatten().copied());
        }
        for child in inner {
            cost.contain(self.cost(child, depth + 1)?);
            check_expanded(&cost)?;
        }
        for id in self.references(node, tag) {
            let (plain, markers) = self.id_cost(id, depth + 1)?;
            cost.refer(plain, cost.elements);
            cost.refer(markers, cost.vertices);
            check_expanded(&cost)?;
        }
        self.seen.insert(node.id(), Seen::Done(cost));
        Ok(cost)
    }

    /// The ids `node` refers to, other than what a `<use>` copies.
    fn references(&self, node: roxmltree::Node<'a, 'input>, tag: &str) -> Vec<&'a str> {
        // A stop's paint isn't drawn, and a link's href isn't followed.
        if tag == "stop" {
            return Vec::new();
        }
        let mut refs = Vec::new();
        for attr in node.attributes() {
            if attr.name() == "href" {
                if !matches!(tag, "use" | "a") {
                    refs.extend(local_iri(attr.value()));
                }
            } else {
                refs.extend(func_iris(attr.value()));
            }
        }
        for (selector, ids) in &self.rules {
            if selector.matches(&XmlNode(node)) {
                refs.extend(ids.iter().copied());
            }
        }
        refs
    }

    /// What the elements `id` names cost: those that aren't markers, and
    /// those that are, which are copied per vertex.
    fn id_cost(&mut self, id: &'a str, depth: usize) -> Result<(Cost, Cost), String> {
        if let Some(&(plain, markers)) = self.id_costs.get(id) {
            if depth + plain.depth.max(markers.depth) > MAX_EXPANDED_DEPTH {
                return Err(format!("{DIR}/{FILE} nests too deep"));
            }
            return Ok((plain, markers));
        }
        let (mut plain, mut markers) = (Cost::default(), Cost::default());
        for target in self.ids.get(id).cloned().unwrap_or_default() {
            let cost = self.cost(target, depth)?;
            let into = if target.tag_name().name() == "marker" {
                &mut markers
            } else {
                &mut plain
            };
            into.refer(cost, 1);
            into.depth = into.depth.max(cost.depth);
            check_expanded(into)?;
        }
        self.id_costs.insert(id, (plain, markers));
        Ok((plain, markers))
    }
}

fn check_expanded(cost: &Cost) -> Result<(), String> {
    if cost.nodes > MAX_EXPANDED_NODES || cost.path_bytes > MAX_EXPANDED_PATH_BYTES {
        return Err(format!("{DIR}/{FILE} expands to too many shapes"));
    }
    Ok(())
}

/// The id in an `href` like `#id`, as usvg reads it.
fn local_iri(value: &str) -> Option<&str> {
    let id = value.trim_start().strip_prefix('#')?;
    let id = id.split(' ').next().unwrap_or_default();
    (!id.is_empty()).then_some(id)
}

/// The ids in every `url(#id)` in `text`, quoted or not, as usvg reads them.
fn func_iris(text: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    let mut from = 0;
    while let Some(at) = text.as_bytes()[from..]
        .windows(4)
        .position(|w| w.eq_ignore_ascii_case(b"url("))
    {
        // Past ASCII, so on a char boundary.
        from += at + 4;
        let rest = text[from..].trim_start();
        let (quote, rest) = match rest.as_bytes().first() {
            Some(&q @ (b'\'' | b'"')) => (Some(char::from(q)), rest[1..].trim_start()),
            _ => (None, rest),
        };
        let Some(rest) = rest.strip_prefix('#') else {
            continue;
        };
        let end = match quote {
            Some(q) => rest.find(q),
            None => rest.find([' ', ')']),
        };
        let id = rest[..end.unwrap_or(rest.len())].trim_end();
        if !id.is_empty() {
            ids.push(id);
        }
    }
    ids
}

/// An element as usvg matches style rules against it.
struct XmlNode<'a, 'input>(roxmltree::Node<'a, 'input>);

impl simplecss::Element for XmlNode<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        self.0.parent_element().map(XmlNode)
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.0.prev_sibling_element().map(XmlNode)
    }

    fn has_local_name(&self, local_name: &str) -> bool {
        self.0.tag_name().name() == local_name
    }

    fn attribute_matches(&self, local_name: &str, operator: simplecss::AttributeOperator) -> bool {
        self.0
            .attribute(local_name)
            .is_some_and(|value| operator.matches(value))
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass) -> bool {
        matches!(class, simplecss::PseudoClass::FirstChild) && self.prev_sibling_element().is_none()
    }
}

/// Refuses a tree with filters, with more than [`MAX_NODES`] nodes, or with
/// groups nested more than [`MAX_DEPTH`] deep, counting clip paths, masks
/// and patterns wherever they are used.
///
/// A filter can take any amount of work per pixel (feTurbulence's
/// numOctaves, feConvolveMatrix's order, feMorphology's radius), all on a
/// drawing that can't be stopped, and an icon doesn't need one.
fn check_complexity(tree: &usvg::Tree) -> Result<(), String> {
    if !tree.filters().is_empty() {
        return Err(format!("{DIR}/{FILE} has filters, which aren't supported"));
    }
    check_group(tree.root(), 1, &mut 0)
}

fn check_group(group: &usvg::Group, depth: usize, nodes: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("{DIR}/{FILE} nests too deep"));
    }
    for node in group.children() {
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err(format!("{DIR}/{FILE} has too many shapes"));
        }
        // A group's clip path and mask, a path's patterns.
        let mut result = Ok(());
        node.subroots(|g| {
            if result.is_ok() {
                result = check_group(g, depth + 1, nodes);
            }
        });
        result?;
        if let usvg::Node::Group(g) = node {
            check_group(g, depth + 1, nodes)?;
        }
    }
    Ok(())
}

/// The text with every `currentColor`, however it is capitalised (CSS
/// keywords aren't case-sensitive, usvg is), spelled the way usvg knows.
/// `None` when it has none.
fn spell_current_color(text: &str) -> Option<String> {
    const WORD: &str = "currentColor";
    let mut bytes = text.as_bytes().to_vec();
    let mut found = false;
    let mut i = 0;
    while i + WORD.len() <= bytes.len() {
        if bytes[i..i + WORD.len()].eq_ignore_ascii_case(WORD.as_bytes()) {
            bytes[i..i + WORD.len()].copy_from_slice(WORD.as_bytes());
            found = true;
            i += WORD.len();
        } else {
            i += 1;
        }
    }
    // Only ASCII letters changed, for ASCII letters: still UTF-8.
    found.then(|| String::from_utf8(bytes).unwrap_or_else(|_| text.to_string()))
}

/// Straight RGBA, `size`×`size`: the SVG's own size (its `viewBox`, or
/// `width` and `height`) scaled to fit with its aspect ratio kept, centred.
fn render_at(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("no memory for the icon")?;
    let (w, h) = (tree.size().width(), tree.size().height());
    let n = size as f32;
    let scale = (n / w).min(n / h);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(format!("{DIR}/{FILE} has no size"));
    }
    let (w, h) = (w * scale, h * scale);
    let (dx, dy) = ((n - w) / 2.0, (n - h) / 2.0);
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    clip(&mut pixmap, dx, dy, w, h);
    Ok(pixmap.take_demultiplied())
}

/// Clears what was drawn outside `x`, `y`, `w`, `h`, the picture's own
/// area: viewers clip an SVG to its viewport, usvg doesn't. A pixel it
/// covers in part keeps that part.
fn clip(pixmap: &mut tiny_skia::Pixmap, x: f32, y: f32, w: f32, h: f32) {
    let cover = |i: usize, start: f32, len: f32| {
        let (from, to) = (i as f32, i as f32 + 1.0);
        (to.min(start + len) - from.max(start)).clamp(0.0, 1.0)
    };
    let width = pixmap.width() as usize;
    let columns: Vec<f32> = (0..width).map(|i| cover(i, x, w)).collect();
    // Premultiplied, so every channel scales alike.
    for (row, line) in pixmap.data_mut().chunks_exact_mut(width * 4).enumerate() {
        let rows = cover(row, y, h);
        for (px, columns) in line.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let part = rows * columns;
            if part < 1.0 {
                for c in px {
                    *c = (f32::from(*c) * part).round() as u8;
                }
            }
        }
    }
}

/// Keeps each pixel's alpha and gives it `color`.
fn tint(mut rgba: Vec<u8>, color: [u8; 3]) -> Vec<u8> {
    for px in rgba.as_chunks_mut::<4>().0.iter_mut() {
        px[..3].copy_from_slice(&color);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREEN: [u8; 3] = [46, 160, 67];
    const BLUE: [u8; 3] = [0, 0, 255];

    fn render_ico(svg: &[u8], tone: Tone) -> Result<Vec<u8>, Error> {
        Renderer::default().render("test", svg, tone)
    }

    fn draw(svg: &str, tone: Tone, size: u32) -> Vec<u8> {
        let all = render_all(svg.as_bytes(), tone.color(), &FOLDER_SIZES).unwrap();
        all.into_iter().find(|(s, _)| *s == size).unwrap().1
    }

    fn px(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        rgba[i..i + 4].try_into().unwrap()
    }

    fn opaque_colors(rgba: &[u8]) -> std::collections::HashSet<[u8; 3]> {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    /// A blue square on the left, and on the right a square in
    /// `currentColor` drawn the way `right` says.
    fn halves(right: &str) -> String {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16">
                <style>.cc {{ fill: currentColor }}</style>
                <rect width="16" height="16" fill="#0000ff"/>
                {right}
            </svg>"##
        )
    }

    #[test]
    fn current_color_takes_the_tone_and_other_colours_stay() {
        let fill = r#"<rect x="16" width="16" height="16" fill="currentColor"/>"#;
        let stroke = r#"<rect x="20" y="4" width="8" height="8" fill="none"
                         stroke="currentColor" stroke-width="8"/>"#;
        let css_class = r#"<rect class="cc" x="16" width="16" height="16"/>"#;
        let css_style = r#"<rect style="fill: CurrentColor" x="16" width="16" height="16"/>"#;
        // The file's own `color`, however it is set, doesn't win.
        let own_color = r#"<g color="red" style="color: #123456 !important">
                             <rect x="16" width="16" height="16" fill="currentColor"/></g>"#;
        for (name, right) in [
            ("fill", fill),
            ("stroke", stroke),
            ("css class", css_class),
            ("css style", css_style),
            ("own color", own_color),
        ] {
            for tone in Tone::ALL {
                let rgba = draw(&halves(right), tone, 32);
                // 32 px across, 16 tall, centred: rows 8 to 23.
                assert_eq!(px(&rgba, 32, 4, 16), [0, 0, 255, 255], "{name} {tone:?}");
                let [r, g, b] = tone.color();
                assert_eq!(px(&rgba, 32, 24, 16), [r, g, b, 255], "{name} {tone:?}");
                assert_eq!(
                    opaque_colors(&rgba),
                    [BLUE, tone.color()].into(),
                    "{name} {tone:?}"
                );
            }
        }
        // Stops, being paints too.
        let gradient = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">
            <linearGradient id="g"><stop offset="0" stop-color="currentColor"/>
            <stop offset="1" stop-color="currentColor"/></linearGradient>
            <rect width="8" height="8" fill="url(#g)"/></svg>"##;
        assert_eq!(
            opaque_colors(&draw(gradient, Tone::Synced, 16)),
            [GREEN].into()
        );
    }

    #[test]
    fn no_current_color_is_a_tinted_silhouette() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">
            <circle cx="8" cy="8" r="7" fill="#0000ff"/>
            <rect x="6" y="6" width="4" height="4" fill="#ffffff" opacity="0.5"/></svg>"##;
        for tone in Tone::ALL {
            for size in FOLDER_SIZES {
                let rgba = draw(svg, tone, size);
                let alphas: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
                assert!(
                    rgba.as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p[..3] == tone.color()),
                    "{tone:?} {size}: every pixel is the tone"
                );
                // The shape and its soft edge stay.
                assert_eq!(alphas[0], 0, "{size}: corner");
                let centre = ((size / 2) * size + size / 2) as usize;
                assert_eq!(alphas[centre], 255, "{size}: centre");
                assert!(alphas.iter().any(|&a| a > 0 && a < 255), "{size}: edges");
            }
        }
    }

    #[test]
    fn keeps_aspect_and_centres() {
        // Wide: a full-bleed rect 2:1 fills the middle half of the rows.
        let wide = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100">
            <rect width="200" height="100" fill="currentColor"/></svg>"#;
        // Tall, sized by width and height without a viewBox.
        let tall = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="40">
            <rect width="10" height="40" fill="currentColor"/></svg>"#;
        for size in FOLDER_SIZES {
            let rgba = draw(wide, Tone::Local, size);
            let alpha = |x, y| px(&rgba, size, x, y)[3];
            let (top, bottom) = (size / 4, size * 3 / 4);
            for x in [0, size / 2, size - 1] {
                assert_eq!(alpha(x, top - 1), 0, "{size}: above at {x}");
                assert_eq!(alpha(x, top), 255, "{size}: top row at {x}");
                assert_eq!(alpha(x, bottom - 1), 255, "{size}: bottom row at {x}");
                assert_eq!(alpha(x, bottom), 0, "{size}: below at {x}");
            }
            // 1:4, so its edges may fall inside a pixel (7.5 to 12.5 at 20).
            let rgba = draw(tall, Tone::Local, size);
            let alpha = |x: f32, y| px(&rgba, size, x as u32, y)[3];
            let (left, right) = (size as f32 * 3.0 / 8.0, size as f32 * 5.0 / 8.0);
            assert_eq!(alpha(left.floor() - 1.0, size / 2), 0, "{size}: left of it");
            assert_eq!(alpha(left.ceil(), 0), 255, "{size}: its left column");
            assert_eq!(
                alpha(right.floor() - 1.0, size - 1),
                255,
                "{size}: its right column"
            );
            assert_eq!(alpha(right.ceil(), size / 2), 0, "{size}: right of it");
            let edge = alpha(left.floor(), size / 2);
            assert!(
                left.fract() == 0.0 || (edge > 0 && edge < 255),
                "{size}: soft edge"
            );
        }
    }

    #[test]
    fn bad_svgs_are_errors_not_panics() {
        let big = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" height="1"/>{}</svg>"#,
            " ".repeat(MAX_BYTES as usize)
        );
        let nested = |depth: usize| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg">{}<rect width="1" height="1"/>{}</svg>"#,
                r#"<g opacity="0.9">"#.repeat(depth),
                "</g>".repeat(depth)
            )
        };
        let many = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg">{}</svg>"#,
            r#"<rect width="1" height="1"/>"#.repeat(MAX_NODES + 1)
        );
        // `<use>` chains nest in usvg without nesting in the XML: usvg
        // stops them at 1024 levels, on a stack that has room for that.
        let chain = |links: usize| {
            let mut defs = String::from(r#"<rect id="u0" width="1" height="1"/>"#);
            for i in 1..=links {
                defs += &format!(r##"<g id="u{i}"><use href="#u{}"/></g>"##, i - 1);
            }
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg"><defs>{defs}</defs><use href="#u{links}"/></svg>"##
            )
        };
        // Within the limits, it draws.
        assert!(render_ico(nested(MAX_DEPTH - 2).as_bytes(), Tone::Synced).is_ok());
        for (name, svg) in [
            ("not xml", "hello".to_string()),
            ("not svg", "<html><body/></html>".to_string()),
            (
                "truncated",
                r#"<svg xmlns="http://www.w3.org/2000/svg"><rect"#.into(),
            ),
            (
                "empty",
                r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#.into(),
            ),
            (
                "zero size",
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 0 0"><rect/></svg>"#.into(),
            ),
            ("too big", big),
            ("too deep", nested(MAX_DEPTH)),
            ("far too deep", nested(5000)),
            ("too many", many),
            ("long use chain", chain(1100)),
            (
                "entities",
                r#"<!DOCTYPE svg [<!ENTITY r "<rect width='1' height='1'/>">]>
                <svg xmlns="http://www.w3.org/2000/svg">&r;</svg>"#
                    .into(),
            ),
            ("use chain", chain(MAX_DEPTH)),
        ] {
            let result = render_ico(svg.as_bytes(), Tone::Synced);
            assert!(result.is_err_and(|e| !e.transient), "{name}");
        }
        assert!(
            render_ico(&[0x1f, 0x8b, 0, 0], Tone::Synced).is_err(),
            "svgz"
        );
        assert!(
            render_ico(b"\xff\xfe<svg/>", Tone::Synced).is_err(),
            "UTF-16"
        );
    }

    #[test]
    fn markup_depth_counts_what_the_parser_nests() {
        let depth = |text: &str| markup_depth(text).unwrap();
        assert_eq!(depth(""), 0);
        assert_eq!(depth("<svg/>"), 0);
        assert_eq!(depth("<svg><g><rect/></g><g/></svg>"), 2);
        assert_eq!(depth("<svg><g><g><g></g></g></g></svg>"), 4);
        // Markup that isn't elements.
        assert_eq!(
            depth("<?xml version='1.0'?><!-- <g><g> --><svg><![CDATA[<g><g>]]><g/></svg>"),
            1
        );
        // `>` and `/>` inside attribute values.
        assert_eq!(depth(r#"<svg a="x>y" b='/>'><g c="/"></g></svg>"#), 2);
        // A doctype line is fine, an internal subset isn't.
        let doctype = r#"<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN"
            "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd"><svg><g/></svg>"#;
        assert_eq!(depth(doctype), 1);
        assert!(markup_depth("<!DOCTYPE svg [<!ENTITY a 'b'>]><svg/>").is_err());
        // Unclosed or stray tags only ever count high.
        assert_eq!(depth("<svg><g"), 2);
        assert_eq!(depth("</g></g><svg><g></g></svg>"), 2);

        // A doctype line still draws.
        let svg = format!(
            r#"<?xml version="1.0"?>{}"#,
            doctype.replace(
                "<svg><g/></svg>",
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>"#
            )
        );
        assert!(render_ico(svg.as_bytes(), Tone::Synced).is_ok());
    }

    #[test]
    fn nothing_outside_the_file_is_loaded() {
        let dir = std::env::temp_dir().join(format!("gitstat-svg-href-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.svg");
        let square = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
            <rect width="8" height="8"/></svg>"#;
        std::fs::write(&other, square).unwrap();
        let data = format!(
            "data:image/svg+xml;utf8,{}",
            square
                .replace('<', "%3C")
                .replace('>', "%3E")
                .replace('"', "'")
        );
        for href in [
            other.to_string_lossy().into_owned(),
            "other.svg".into(),
            data,
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">
                    <image href="{href}" width="8" height="8"/></svg>"#
            );
            let tree = usvg::Tree::from_str(&svg, &options(GREEN)).unwrap();
            assert!(!tree.root().has_children(), "{href}");
            assert!(render_ico(svg.as_bytes(), Tone::Synced).is_err(), "{href}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_readme_example_draws() {
        // Git for Windows checks README.md out with CRLF under core.autocrlf.
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
            .unwrap()
            .replace("\r\n", "\n");
        let start = readme.find("```svg\n").expect("an svg example") + 7;
        let svg = &readme[start..start + readme[start..].find("```").unwrap()];
        let rgba = draw(svg, Tone::Local, 48);
        // A disc in the tone with a white mark, on transparent corners.
        assert_eq!(px(&rgba, 48, 0, 0)[3], 0);
        let [r, g, b] = Tone::Local.color();
        assert_eq!(px(&rgba, 48, 24, 40), [r, g, b, 255]);
        assert!(opaque_colors(&rgba).contains(&[255, 255, 255]));
    }

    #[test]
    fn render_ico_has_every_folder_size() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
            <path d="M2 12 L12 2 L22 12 L12 22 Z" fill="currentColor"/></svg>"#;
        let ico = render_ico(svg.as_bytes(), Tone::Attention).unwrap();
        assert_eq!(
            u16::from_le_bytes([ico[4], ico[5]]) as usize,
            FOLDER_SIZES.len()
        );
        let images: Vec<_> = FOLDER_SIZES
            .iter()
            .map(|&s| (s, draw(svg, Tone::Attention, s)))
            .collect();
        assert_eq!(ico, icon::encode_ico(&images));
        assert_eq!(ico, render_ico(svg.as_bytes(), Tone::Attention).unwrap());
    }

    #[test]
    fn file_names_follow_repo_tone_and_svg() {
        let (a, b) = (Path::new("/dev/a"), Path::new("/dev/b"));
        let name = file_name(a, Tone::Local, b"<svg/>");
        assert!(
            name.starts_with("repo-") && name.contains("-amber-"),
            "{name}"
        );
        assert!(name.ends_with(".ico") && is_repo_icon_file(&name));
        assert_eq!(name, file_name(a, Tone::Local, b"<svg/>"), "stable");
        let others = [
            file_name(b, Tone::Local, b"<svg/>"),
            file_name(a, Tone::Synced, b"<svg/>"),
            file_name(a, Tone::Attention, b"<svg/>"),
            file_name(a, Tone::Local, b"<svg />"),
        ];
        let all: std::collections::HashSet<_> = others.iter().chain([&name]).collect();
        assert_eq!(all.len(), 5);
        // FNV-1a's published test vector: the hash never changes.
        assert_eq!(fnv(&[b"a"]), 0xaf63_dc4c_8601_ec8c);
        assert!(is_repo_icon_file("repo-1-green-2.ico.tmp"));
        assert!(!is_repo_icon_file("synced-v2.ico"));
        assert!(!is_repo_icon_file("repo-1-green-2.png"));
    }

    /// The review's fan-out: one path of `segments` segments, used through
    /// `levels` levels of ten `<use>`s each.
    fn fan_out(levels: usize, segments: usize) -> String {
        let mut d = String::from("M0 0");
        for i in 0..segments {
            d += &format!(" L{} {}", i % 30, (i * 7) % 30);
        }
        let mut svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><path id="l0" d="{d}" fill="currentColor"/>"#
        );
        for level in 1..=levels {
            svg += &format!(r#"<g id="l{level}">"#);
            for _ in 0..10 {
                svg += &format!(r##"<use href="#l{}"/>"##, level - 1);
            }
            svg += "</g>";
        }
        svg + &format!(r##"</defs><use href="#l{levels}"/></svg>"##)
    }

    fn expansion(svg: &str) -> Result<(), String> {
        check_expansion(&roxmltree::Document::parse(svg).unwrap())
    }

    fn svg8(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">{body}</svg>"#)
    }

    #[test]
    fn expansion_is_refused_before_usvg_builds_it() {
        let start = std::time::Instant::now();
        let rect = r#"<rect width="1" height="1"/>"#;
        let rects = |n: usize| rect.repeat(n);
        let pattern = format!(
            r#"<pattern id="p" width="1" height="1">{}</pattern>"#,
            rects(200)
        );
        let gradient = format!(
            r#"<linearGradient id="g">{}</linearGradient>"#,
            r#"<stop offset="0"/>"#.repeat(300)
        );
        let marker = format!(r#"<marker id="m">{}</marker>"#, rects(50));
        let many = |attrs: &str| format!(r#"<rect width="1" height="1" {attrs}/>"#).repeat(600);
        let cycle = |kind: &str, attr: &str, value: &str| {
            ["a", "b", "c"]
                .iter()
                .zip(["b", "c", "a"])
                .map(|(id, next)| {
                    format!(
                        r#"<{kind} id="{id}"><rect width="1" height="1" {attr}="{}"/></{kind}>"#,
                        value.replace("ID", next)
                    )
                })
                .collect::<String>()
                + &format!(
                    r#"<rect width="8" height="8" {attr}="{}"/>"#,
                    value.replace("ID", "a")
                )
        };
        for (name, svg, why) in [
            // 40 KiB that usvg takes seconds and a GiB to build.
            (
                "long path, used 10⁴ times",
                fan_out(4, 10_000),
                "too many shapes",
            ),
            ("10⁵ copies", fan_out(5, 0), "too many shapes"),
            // Each shape gets its own copy of a pattern or gradient...
            (
                "pattern",
                svg8(&(pattern.clone() + &many(r#"fill="url(#p)""#))),
                "too many shapes",
            ),
            (
                "gradient",
                svg8(&(gradient + &many(r#"stroke="url('#g')""#))),
                "too many shapes",
            ),
            (
                "inherited pattern",
                svg8(&format!(r#"{pattern}<g fill="url(#p)">{}</g>"#, rects(600))),
                "too many shapes",
            ),
            (
                "pattern from a style sheet",
                svg8(&format!(
                    "<style>.x {{ fill: url(#p) }}</style>{pattern}{}",
                    many(r#"class="x""#)
                )),
                "too many shapes",
            ),
            (
                "pattern from a style attribute",
                svg8(&(pattern + &many(r#"style="fill: url( #p )""#))),
                "too many shapes",
            ),
            // ...and a marker is copied to every vertex.
            (
                "marker",
                svg8(&format!(
                    r#"{marker}<path d="M0 0{}" marker-mid="url(#m)"/>"#,
                    " L1 1".repeat(500)
                )),
                "too many shapes",
            ),
            // usvg follows these until its stack overflows.
            (
                "pattern loop",
                svg8(&cycle("pattern", "fill", "url(#ID)")),
                "refers to itself",
            ),
            (
                "clip loop",
                svg8(&cycle("clipPath", "clip-path", "url(#ID)")),
                "refers to itself",
            ),
            (
                "use loop",
                svg8(
                    r##"<g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g><use href="#a"/>"##,
                ),
                "refers to itself",
            ),
        ] {
            let err = expansion(&svg).unwrap_err();
            assert!(err.contains(why), "{name}: {err}");
            let drawn = render_ico(svg.as_bytes(), Tone::Synced).unwrap_err();
            assert!(
                !drawn.transient && drawn.message.contains(why),
                "{name}: {drawn:?}"
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );

        // A few copies are fine.
        let svg = fan_out(2, 100);
        expansion(&svg).unwrap();
        assert!(render_ico(svg.as_bytes(), Tone::Synced).is_ok());
    }

    #[test]
    fn exported_drawings_still_draw() {
        // As Illustrator writes them: paint and clips from classes.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">
            <defs><style>.cls-1{fill:url(#linear-gradient);}.cls-2{clip-path:url(#clip-path);}</style>
            <linearGradient id="linear-gradient" x1="0" y1="0" x2="16" y2="0" gradientUnits="userSpaceOnUse">
            <stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient>
            <clipPath id="clip-path"><circle cx="8" cy="8" r="8"/></clipPath>
            <symbol id="dot" viewBox="0 0 2 2"><circle cx="1" cy="1" r="1" fill="currentColor"/></symbol></defs>
            <g class="cls-2"><rect class="cls-1" width="16" height="16"/>
            <use href="#dot" x="4" y="7" width="2" height="2"/>
            <use href="#dot" x="10" y="7" width="2" height="2"/></g>
            </svg>"##;
        expansion(svg).unwrap();
        let rgba = draw(svg, Tone::Synced, 32);
        assert_eq!(px(&rgba, 32, 0, 0)[3], 0, "clipped");
        let colors = opaque_colors(&rgba);
        assert!(colors.len() > 10, "a gradient: {}", colors.len());
        assert!(colors.contains(&GREEN), "the dots");
    }

    #[test]
    fn filters_are_refused() {
        let start = std::time::Instant::now();
        for body in [
            // Days of work per pixel.
            r#"<filter id="f"><feTurbulence baseFrequency="0.5" numOctaves="2000000000"/></filter>
               <rect width="8" height="8" filter="url(#f)"/>"#,
            r#"<filter id="f"><feGaussianBlur stdDeviation="1"/></filter>
               <g filter="url(#f)"><rect width="8" height="8"/></g>"#,
            r#"<rect width="8" height="8" style="filter: blur(1px)"/>"#,
            // In a pattern's content.
            r#"<filter id="f"><feFlood flood-color="red"/></filter>
               <pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse">
               <rect width="8" height="8" filter="url(#f)"/></pattern>
               <rect width="8" height="8" fill="url(#p)"/>"#,
        ] {
            let err = render_ico(svg8(body).as_bytes(), Tone::Synced).unwrap_err();
            assert!(
                err.message.contains("filters") && !err.transient,
                "{body}: {err:?}"
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn nothing_shows_outside_the_view_box() {
        // Wide, with a rect running past it above and below.
        let wide = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16">
            <rect y="-16" width="32" height="48" fill="currentColor"/></svg>"#;
        for size in FOLDER_SIZES {
            let rgba = draw(wide, Tone::Synced, size);
            let alpha = |x, y| px(&rgba, size, x, y)[3];
            let (top, bottom) = (size / 4, size * 3 / 4);
            for x in [0, size / 2, size - 1] {
                assert_eq!(alpha(x, 0), 0, "{size}: top edge at {x}");
                assert_eq!(alpha(x, top - 1), 0, "{size}: above at {x}");
                assert_eq!(alpha(x, top), 255, "{size}: top row at {x}");
                assert_eq!(alpha(x, bottom - 1), 255, "{size}: bottom row at {x}");
                assert_eq!(alpha(x, bottom), 0, "{size}: below at {x}");
                assert_eq!(alpha(x, size - 1), 0, "{size}: bottom edge at {x}");
            }
        }
        // Tall, running past it left and right. At 20 px it spans 7.5 to
        // 12.5: the pixels it half covers keep half.
        let tall = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 40">
            <rect x="-10" width="30" height="40" fill="currentColor"/></svg>"#;
        let rgba = draw(tall, Tone::Synced, 20);
        let alpha = |x| px(&rgba, 20, x, 10)[3];
        assert_eq!((alpha(6), alpha(10), alpha(13)), (0, 255, 0));
        for x in [7, 12] {
            assert!((120..=135).contains(&alpha(x)), "{x}: {}", alpha(x));
        }
    }

    #[test]
    fn one_drawing_at_a_time_per_repo() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">
            <rect width="8" height="8" fill="currentColor"/></svg>"#;
        let expected = render_ico(svg.as_bytes(), Tone::Synced).unwrap();
        let key = late_key(svg.as_bytes(), Tone::Synced, &FOLDER_SIZES);
        let renderer = Renderer::default();
        let late = || vec![(16, b"late".to_vec())];
        let going = |repo: &str| {
            let late = Late { key, result: None };
            lock(&renderer.late).insert(repo.into(), late);
        };
        // One still going: that repo waits, others don't.
        going("a");
        let err = renderer
            .render("a", svg.as_bytes(), Tone::Synced)
            .unwrap_err();
        assert!(
            err.transient && err.message.contains("still being drawn"),
            "{err:?}"
        );
        assert_eq!(
            renderer.render("b", svg.as_bytes(), Tone::Synced),
            Ok(expected.clone())
        );
        // Past MAX_LATE of them, every repo waits.
        going("b");
        let err = renderer
            .render("c", svg.as_bytes(), Tone::Synced)
            .unwrap_err();
        assert!(
            err.transient && err.message.contains("must wait"),
            "{err:?}"
        );
        // Once one has finished, its result answers the same file and tone,
        // once; another file is drawn afresh.
        lock(&renderer.late).get_mut("a").unwrap().result = Some(Ok(late()));
        assert_eq!(
            renderer.render("a", svg.as_bytes(), Tone::Synced),
            Ok(icon::encode_ico(&late()))
        );
        assert_eq!(
            renderer.render("a", svg.as_bytes(), Tone::Synced),
            Ok(expected.clone())
        );
        lock(&renderer.late).get_mut("b").unwrap().result = Some(Ok(late()));
        assert_eq!(
            renderer
                .render("b", svg.as_bytes(), Tone::Local)
                .map(|i| i.len()),
            Ok(expected.len())
        );
        assert!(lock(&renderer.late).is_empty());
    }

    #[test]
    fn a_drawing_that_takes_too_long_is_kept_when_it_finishes() {
        let svg = fan_out(2, 100);
        let expected = render_ico(svg.as_bytes(), Tone::Attention).unwrap();
        let renderer = Renderer::with_timeout(Duration::ZERO);
        let first = renderer.render("a", svg.as_bytes(), Tone::Attention);
        // Unless it was done at once, it is worth another try...
        if let Err(e) = first {
            assert!(e.transient && e.message.contains("too long"), "{e:?}");
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            // ...which waits for it, then gets what it drew.
            loop {
                match renderer.render("a", svg.as_bytes(), Tone::Attention) {
                    Ok(ico) => {
                        assert_eq!(ico, expected);
                        break;
                    }
                    Err(e) => assert!(e.transient && e.message.contains("still"), "{e:?}"),
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitstat-svg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_and_fingerprint() {
        let repo = temp("load");
        assert_eq!(load(&repo), Ok(None));
        assert_eq!(fingerprint(&repo), None);
        std::fs::create_dir_all(repo.join(DIR)).unwrap();
        assert_eq!(load(&repo), Ok(None));
        std::fs::write(svg_path(&repo), "<svg/>").unwrap();
        assert_eq!(load(&repo), Ok(Some(b"<svg/>".to_vec())));
        let first = fingerprint(&repo).unwrap();
        assert_eq!(first, fingerprint_of(b"<svg/>"));
        // A new drawing, a new fingerprint: every folder is drawn again.
        assert!(first.starts_with(&format!("v{RENDER_VERSION}-")), "{first}");
        assert_ne!(fingerprint_at(1, b"<svg/>"), fingerprint_at(2, b"<svg/>"));
        std::fs::write(svg_path(&repo), "<svg />").unwrap();
        let second = fingerprint(&repo).unwrap();
        assert_ne!(first, second, "new bytes, new fingerprint");
        assert_eq!(fingerprint(&repo).unwrap(), second);

        // Too big: refused unread, with a fingerprint of its own.
        let big = vec![b' '; MAX_BYTES as usize + 1];
        std::fs::write(svg_path(&repo), &big).unwrap();
        let err = load(&repo).unwrap_err();
        assert!(
            err.message.contains("over 1024 KiB") && !err.transient,
            "{err:?}"
        );
        assert!(fingerprint(&repo).unwrap().starts_with("unusable"));
        std::fs::write(svg_path(&repo), &big[1..]).unwrap();
        assert!(load(&repo).unwrap().is_some(), "exactly the limit is fine");

        // A folder named icon.svg is no icon.
        std::fs::remove_file(svg_path(&repo)).unwrap();
        std::fs::create_dir(svg_path(&repo)).unwrap();
        assert_eq!(load(&repo), Ok(None));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let dir = temp("links");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join(FILE), "<svg/>").unwrap();
        let file_link = dir.join("file-link");
        std::fs::create_dir_all(file_link.join(DIR)).unwrap();
        std::os::unix::fs::symlink(elsewhere.join(FILE), svg_path(&file_link)).unwrap();
        let dir_link = dir.join("dir-link");
        std::fs::create_dir_all(&dir_link).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir_link.join(DIR)).unwrap();
        for repo in [file_link, dir_link] {
            let err = load(&repo).unwrap_err();
            assert!(err.message.contains("links") && !err.transient, "{repo:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
