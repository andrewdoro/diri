// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The API surface's data: requests, their query/header/form pairs, the
//! environments their `{{variables}}` come from, saved collections and
//! history. Ported from Ely's `devtools::{request, environment, collection}`
//! and extended with what a working client needs: the URL and the Params
//! table stay in sync, variables nest, and disabled rows are kept.
//!
//! Everything here is plain data with no GPUI types, so it serializes to the
//! per-project store (`storage`) and is tested without a window.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// An HTTP method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub const ALL: [Method; 7] = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
        Method::Head,
        Method::Options,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }

    pub fn parse(name: &str) -> Option<Method> {
        Method::ALL
            .into_iter()
            .find(|method| method.name().eq_ignore_ascii_case(name.trim()))
    }

    /// Whether a body usually travels with it; picks the Body tab's default.
    pub const fn carries_body(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

/// One row of a key/value table: a query parameter, header or form field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    pub name: String,
    pub value: String,
}

fn enabled_default() -> bool {
    true
}

impl Pair {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            enabled: true,
            name: name.into(),
            value: value.into(),
        }
    }

    /// A row that goes out: switched on and named.
    pub fn is_live(&self) -> bool {
        self.enabled && !self.name.trim().is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    #[default]
    None,
    Json,
    Form,
    Raw,
}

impl BodyKind {
    pub const ALL: [BodyKind; 4] = [
        BodyKind::None,
        BodyKind::Json,
        BodyKind::Form,
        BodyKind::Raw,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            BodyKind::None => "None",
            BodyKind::Json => "JSON",
            BodyKind::Form => "Form",
            BodyKind::Raw => "Raw",
        }
    }

    /// The `Content-Type` sent when the person set none.
    pub const fn content_type(self) -> Option<&'static str> {
        match self {
            BodyKind::None => None,
            BodyKind::Json => Some("application/json"),
            BodyKind::Form => Some("application/x-www-form-urlencoded"),
            BodyKind::Raw => Some("text/plain; charset=utf-8"),
        }
    }
}

/// How a request signs in. Its secrets live in the request (or, better, in a
/// secret `{{variable}}`) and are masked in the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Auth {
    #[default]
    None,
    Bearer {
        token: String,
    },
    Basic {
        user: String,
        password: String,
    },
}

/// A request as written, `{{variables}}` and all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRequest {
    pub id: String,
    pub name: String,
    pub method: Method,
    /// The address, query included. Its query and the live rows of
    /// `params` always agree (`set_url`, `set_params`).
    pub url: String,
    /// The Params table: the query's pairs, plus rows that do not go out
    /// (switched off, or not named yet).
    #[serde(default)]
    pub params: Vec<Pair>,
    #[serde(default)]
    pub headers: Vec<Pair>,
    #[serde(default)]
    pub body_kind: BodyKind,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub form: Vec<Pair>,
    #[serde(default)]
    pub auth: Auth,
}

impl ApiRequest {
    /// A request whose Params table is read from `url`.
    pub fn new(id: impl Into<String>, method: Method, url: impl Into<String>) -> Self {
        let mut request = Self {
            id: id.into(),
            method,
            ..Self::default()
        };
        request.set_url(url);
        request
    }

    /// The person retyped the URL: its query becomes the live Params; rows
    /// that do not go out stay, after them.
    pub fn set_url(&mut self, url: impl Into<String>) {
        self.url = url.into();
        let mut params = query_pairs(&self.url);
        params.extend(self.params.iter().filter(|pair| !pair.is_live()).cloned());
        self.params = params;
    }

    /// The person edited the Params table: its live rows become the URL's
    /// query. The path and fragment are untouched.
    pub fn set_params(&mut self, params: Vec<Pair>) {
        self.url = with_query(&self.url, &params);
        self.params = params;
    }

    /// Repairs a request read from disk whose table and URL disagree.
    pub fn normalize(&mut self) {
        let live: Vec<Pair> = self
            .params
            .iter()
            .filter(|pair| pair.is_live())
            .cloned()
            .collect();
        if live != query_pairs(&self.url) {
            let url = self.url.clone();
            self.set_url(url);
        }
    }

    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        let path = strip_scheme(&self.url);
        if path.is_empty() {
            "Untitled request".to_owned()
        } else {
            path.to_owned()
        }
    }

    /// Every `{{name}}` the request uses, in first-use order.
    pub fn variable_names(&self) -> Vec<String> {
        let mut texts = vec![self.url.as_str(), self.body.as_str()];
        for pair in self
            .headers
            .iter()
            .chain(self.form.iter())
            .filter(|pair| pair.enabled)
        {
            texts.push(&pair.name);
            texts.push(&pair.value);
        }
        match &self.auth {
            Auth::None => {}
            Auth::Bearer { token } => texts.push(token),
            Auth::Basic { user, password } => {
                texts.push(user);
                texts.push(password);
            }
        }
        let mut seen = HashSet::new();
        texts
            .into_iter()
            .flat_map(placeholders)
            .filter(|name| seen.insert(name.clone()))
            .collect()
    }
}

fn strip_scheme(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

/// The `{{name}}` placeholders in `text`, trimmed, in order.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let name = rest[open + 2..open + 2 + close].trim();
        if !name.is_empty() {
            names.push(name.to_owned());
        }
        rest = &rest[open + 2 + close + 2..];
    }
    names
}

/// A variable of an environment. A secret's value is masked until revealed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variable {
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

impl Variable {
    pub fn new(name: impl Into<String>, value: impl Into<String>, secret: bool) -> Self {
        Self {
            enabled: true,
            name: name.into(),
            value: value.into(),
            secret,
        }
    }
}

/// A named set of variables a request fills its `{{names}}` from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub variables: Vec<Variable>,
}

impl Environment {
    pub fn lookup(&self, name: &str) -> Option<&Variable> {
        self.variables
            .iter()
            .rev()
            .find(|variable| variable.enabled && variable.name.trim() == name)
    }

    /// Sets each variable, replacing a same-named one's value.
    pub fn merge(&mut self, variables: impl IntoIterator<Item = Variable>) {
        for variable in variables {
            match self
                .variables
                .iter_mut()
                .find(|existing| existing.name == variable.name)
            {
                Some(existing) => {
                    existing.value = variable.value;
                    existing.secret |= variable.secret;
                    existing.enabled = true;
                }
                None => self.variables.push(variable),
            }
        }
    }
}

/// What `{{name}}` stands for in text that went out, or why it could not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    /// Names with no value, left as written.
    pub missing: Vec<String>,
    /// Names whose values refer back to themselves.
    pub cyclic: Vec<String>,
    /// Whether a secret's value went into `text`. A secret's value is never
    /// shown in the preview line; the request still sends it.
    pub uses_secret: bool,
}

/// How deep a variable's value may refer to other variables.
const MAX_NESTING: usize = 8;

/// `text` with each `{{name}}` swapped for its value. A value may itself use
/// `{{other}}` names, up to [`MAX_NESTING`] deep; a name with no value, or one
/// that refers back to itself, stays as written and is reported.
pub fn resolve(text: &str, environment: Option<&Environment>) -> Resolved {
    let mut resolved = Resolved::default();
    let mut stack = Vec::new();
    resolved.text = expand(text, environment, &mut stack, &mut resolved);
    resolved
}

fn expand(
    text: &str,
    environment: Option<&Environment>,
    stack: &mut Vec<String>,
    resolved: &mut Resolved,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let end = open + 2 + close + 2;
        let name = rest[open + 2..open + 2 + close].trim();
        out.push_str(&rest[..open]);
        let written = &rest[open..end];
        match environment.and_then(|environment| environment.lookup(name)) {
            Some(_) if stack.iter().any(|seen| seen == name) || stack.len() >= MAX_NESTING => {
                if !resolved.cyclic.iter().any(|seen| seen == name) {
                    resolved.cyclic.push(name.to_owned());
                }
                out.push_str(written);
            }
            Some(variable) => {
                resolved.uses_secret |= variable.secret;
                stack.push(name.to_owned());
                let value = expand(&variable.value, environment, stack, resolved);
                stack.pop();
                out.push_str(&value);
            }
            None => {
                if !name.is_empty() && !resolved.missing.iter().any(|seen| seen == name) {
                    resolved.missing.push(name.to_owned());
                }
                out.push_str(written);
            }
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Text made safe for a URL's query. Unreserved characters and the
/// sub-delimiters a query may carry stay; `&`, `=`, `+`, `#`, `%`, spaces and
/// non-ASCII become `%XX` of their UTF-8 bytes. `{{name}}` placeholders stay
/// as written, so they still resolve after a round trip through the table.
pub fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = rest.find("{{").and_then(|open| {
            rest[open + 2..]
                .find("}}")
                .map(|close| (open, open + 2 + close + 2))
        });
        let (plain, placeholder, tail) = match next {
            Some((open, end)) => (&rest[..open], &rest[open..end], &rest[end..]),
            None => (rest, "", ""),
        };
        for byte in plain.bytes() {
            match byte {
                b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'~'
                | b'!'
                | b'$'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b','
                | b';'
                | b':'
                | b'@'
                | b'/'
                | b'?' => out.push(byte as char),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out.push_str(placeholder);
        if next.is_none() {
            return out;
        }
        rest = tail;
    }
}

/// `%XX` and `+` decoded; malformed escapes and invalid UTF-8 stay as written.
pub fn unpercent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit() =>
            {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Splits a URL into what precedes its query, the query, and the fragment
/// (with its `#`).
fn split_url(url: &str) -> (&str, Option<&str>, &str) {
    let (rest, fragment) = match url.find('#') {
        Some(hash) => (&url[..hash], &url[hash..]),
        None => (url, ""),
    };
    match rest.split_once('?') {
        Some((base, query)) => (base, Some(query), fragment),
        None => (rest, None, fragment),
    }
}

/// The URL's query as decoded pairs, all switched on.
pub fn query_pairs(url: &str) -> Vec<Pair> {
    let (_, query, _) = split_url(url);
    query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((name, value)) => Pair::new(unpercent(name), unpercent(value)),
            None => Pair::new(unpercent(part), ""),
        })
        .collect()
}

/// `url` with its query rebuilt from the live rows of `params`.
pub fn with_query(url: &str, params: &[Pair]) -> String {
    let (base, _, fragment) = split_url(url);
    let query: Vec<String> = params
        .iter()
        .filter(|pair| pair.is_live())
        .map(|pair| {
            if pair.value.is_empty() {
                percent(pair.name.trim())
            } else {
                format!("{}={}", percent(pair.name.trim()), percent(&pair.value))
            }
        })
        .collect();
    if query.is_empty() {
        format!("{base}{fragment}")
    } else {
        format!("{base}?{}{fragment}", query.join("&"))
    }
}

/// An address as typed, made absolute: no scheme means `http://` for a local
/// host and `https://` for anything else.
pub fn absolute_url(url: &str) -> String {
    let url = url.trim();
    if url.contains("://") {
        return url.to_owned();
    }
    let host = url.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(host, _)| host);
    let local = matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".test");
    if local {
        format!("http://{url}")
    } else {
        format!("https://{url}")
    }
}

/// A saved request, or a folder of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Saved {
    Folder {
        id: String,
        name: String,
        #[serde(default)]
        items: Vec<Saved>,
    },
    Request {
        request: ApiRequest,
    },
}

impl Saved {
    pub fn id(&self) -> &str {
        match self {
            Saved::Folder { id, .. } => id,
            Saved::Request { request } => &request.id,
        }
    }
}

/// A row of the collection tree as drawn: its depth and what it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub id: String,
    pub depth: usize,
    pub label: String,
    /// `None` for a folder.
    pub method: Option<Method>,
    pub open: bool,
    pub count: usize,
}

/// The tree's visible rows: every root item, and the items of open folders.
pub fn tree_rows(items: &[Saved], open: &HashSet<String>) -> Vec<TreeRow> {
    fn walk(items: &[Saved], depth: usize, open: &HashSet<String>, rows: &mut Vec<TreeRow>) {
        for item in items {
            match item {
                Saved::Folder { id, name, items } => {
                    let expanded = open.contains(id);
                    rows.push(TreeRow {
                        id: id.clone(),
                        depth,
                        label: name.clone(),
                        method: None,
                        open: expanded,
                        count: items.len(),
                    });
                    if expanded {
                        walk(items, depth + 1, open, rows);
                    }
                }
                Saved::Request { request } => rows.push(TreeRow {
                    id: request.id.clone(),
                    depth,
                    label: request.title(),
                    method: Some(request.method),
                    open: false,
                    count: 0,
                }),
            }
        }
    }
    let mut rows = Vec::new();
    walk(items, 0, open, &mut rows);
    rows
}

pub fn find_request<'a>(items: &'a [Saved], id: &str) -> Option<&'a ApiRequest> {
    items.iter().find_map(|item| match item {
        Saved::Request { request } if request.id == id => Some(request),
        Saved::Request { .. } => None,
        Saved::Folder { items, .. } => find_request(items, id),
    })
}

/// Replaces the saved request with this id, wherever it is. False when it is
/// not saved yet.
pub fn replace_request(items: &mut [Saved], request: &ApiRequest) -> bool {
    items.iter_mut().any(|item| match item {
        Saved::Request { request: saved } if saved.id == request.id => {
            *saved = request.clone();
            true
        }
        Saved::Request { .. } => false,
        Saved::Folder { items, .. } => replace_request(items, request),
    })
}

/// Adds `item` inside the folder `folder`, or at the root when `folder` is
/// `None` or gone.
pub fn insert_into(items: &mut Vec<Saved>, folder: Option<&str>, item: Saved) {
    fn into_folder(items: &mut [Saved], folder: &str, item: &mut Option<Saved>) {
        for each in items {
            if let Saved::Folder { id, items, .. } = each {
                if id == folder {
                    if let Some(item) = item.take() {
                        items.push(item);
                    }
                    return;
                }
                into_folder(items, folder, item);
                if item.is_none() {
                    return;
                }
            }
        }
    }
    let mut item = Some(item);
    if let Some(folder) = folder {
        into_folder(items, folder, &mut item);
    }
    if let Some(item) = item {
        items.push(item);
    }
}

/// Takes the item with this id out of the tree, wherever it lies.
pub fn remove_item(items: &mut Vec<Saved>, id: &str) -> Option<Saved> {
    if let Some(index) = items.iter().position(|item| item.id() == id) {
        return Some(items.remove(index));
    }
    items.iter_mut().find_map(|item| match item {
        Saved::Folder { items, .. } => remove_item(items, id),
        Saved::Request { .. } => None,
    })
}

pub fn rename_folder(items: &mut [Saved], folder: &str, name: &str) -> bool {
    items.iter_mut().any(|item| match item {
        Saved::Folder {
            id, name: current, ..
        } if id == folder => {
            *current = name.to_owned();
            true
        }
        Saved::Folder { items, .. } => rename_folder(items, folder, name),
        Saved::Request { .. } => false,
    })
}

/// A request that went out, as written, with how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub request: ApiRequest,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub took_ms: u64,
    pub at_ms: u64,
}

/// How many sent requests history keeps per project.
pub const HISTORY_LIMIT: usize = 50;

/// Adds `entry` first. Sending the same request again moves it up rather
/// than repeating it, and the oldest beyond [`HISTORY_LIMIT`] fall off.
pub fn record_history(history: &mut Vec<HistoryEntry>, entry: HistoryEntry) {
    history.retain(|existing| {
        existing.request.method != entry.request.method
            || existing.request.url != entry.request.url
            || existing.request.body != entry.request.body
    });
    history.insert(0, entry);
    history.truncate(HISTORY_LIMIT);
}

/// A process-unique id for requests, folders and environments.
pub fn fresh_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    format!("{prefix}-{now:x}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// Text for an `application/x-www-form-urlencoded` body: unreserved
/// characters stay, spaces become `+`, everything else `%XX`.
pub fn form_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Why a request cannot go out yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareError {
    EmptyUrl,
    Missing(Vec<String>),
    Cyclic(Vec<String>),
}

impl PrepareError {
    pub fn message(&self) -> String {
        match self {
            PrepareError::EmptyUrl => "Enter a URL to send".to_owned(),
            PrepareError::Missing(names) => format!("No value for {}", braces(names)),
            PrepareError::Cyclic(names) => format!("{} refers to itself", braces(names)),
        }
    }
}

fn braces(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("{{{{{name}}}}}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The request with every `{{variable}}` filled in, made absolute, with its
/// auth header and default `Content-Type`, ready for `http::send`.
pub fn prepare(
    request: &ApiRequest,
    environment: Option<&Environment>,
) -> Result<super::http::Outgoing, PrepareError> {
    let mut missing: Vec<String> = Vec::new();
    let mut cyclic: Vec<String> = Vec::new();
    let mut fill = |text: &str| {
        let resolved = resolve(text, environment);
        for name in resolved.missing {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        for name in resolved.cyclic {
            if !cyclic.contains(&name) {
                cyclic.push(name);
            }
        }
        resolved.text
    };
    let url = fill(request.url.trim());
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|pair| pair.is_live())
        .map(|pair| (fill(pair.name.trim()), fill(&pair.value)))
        .collect();
    let has = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .any(|(header, _)| header.eq_ignore_ascii_case(name))
    };
    match &request.auth {
        Auth::None => {}
        Auth::Bearer { token } => {
            let token = fill(token);
            if !has(&headers, "authorization") && !token.trim().is_empty() {
                headers.push((
                    "Authorization".to_owned(),
                    format!("Bearer {}", token.trim()),
                ));
            }
        }
        Auth::Basic { user, password } => {
            use base64::Engine as _;
            let credentials = format!("{}:{}", fill(user), fill(password));
            if !has(&headers, "authorization") {
                headers.push((
                    "Authorization".to_owned(),
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(credentials)
                    ),
                ));
            }
        }
    }
    let body = match request.body_kind {
        BodyKind::None => None,
        BodyKind::Json | BodyKind::Raw => Some(fill(&request.body).into_bytes()),
        BodyKind::Form => Some(
            request
                .form
                .iter()
                .filter(|pair| pair.is_live())
                .map(|pair| {
                    format!(
                        "{}={}",
                        form_component(&fill(pair.name.trim())),
                        form_component(&fill(&pair.value))
                    )
                })
                .collect::<Vec<_>>()
                .join("&")
                .into_bytes(),
        ),
    };
    if !cyclic.is_empty() {
        return Err(PrepareError::Cyclic(cyclic));
    }
    if !missing.is_empty() {
        return Err(PrepareError::Missing(missing));
    }
    if url.is_empty() {
        return Err(PrepareError::EmptyUrl);
    }
    if body.is_some()
        && let Some(content_type) = request.body_kind.content_type()
        && !has(&headers, "content-type")
    {
        headers.push(("Content-Type".to_owned(), content_type.to_owned()));
    }
    let mut outgoing = super::http::Outgoing::new(request.method, absolute_url(&url));
    outgoing.headers = headers;
    outgoing.body = body;
    Ok(outgoing)
}

/// A request built from what an agent sent with `open_api_request`.
pub fn from_draft(draft: &diri_proto::ApiRequestDraft) -> ApiRequest {
    let method = Method::parse(&draft.method).unwrap_or_default();
    let mut request = ApiRequest::new(fresh_id("req"), method, draft.url.trim());
    request.name = draft.name.clone().unwrap_or_default();
    request.headers = draft
        .headers
        .iter()
        .map(|header| Pair::new(header.name.clone(), header.value.clone()))
        .collect();
    if let Some(body) = draft.body.as_ref().filter(|body| !body.is_empty()) {
        let content_type = draft
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-type"))
            .map(|header| header.value.to_ascii_lowercase());
        let json = serde_json::from_str::<serde::de::IgnoredAny>(body).is_ok();
        request.body_kind = match content_type.as_deref() {
            Some(kind) if kind.contains("json") => BodyKind::Json,
            Some(kind) if kind.contains("x-www-form-urlencoded") => {
                request.form = query_pairs(&format!("?{body}"));
                BodyKind::Form
            }
            Some(_) => BodyKind::Raw,
            None if json => BodyKind::Json,
            None => BodyKind::Raw,
        };
        if request.body_kind != BodyKind::Form {
            request.body = body.clone();
        }
    }
    request
}
