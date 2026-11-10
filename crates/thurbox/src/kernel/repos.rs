//! What the creation flow needs to know, computed off the render path.
//!
//! Two reads, both parameterised by something only the flow knows — which
//! host, which directory — and neither of them cheap: a bookmark list is a
//! query, and a directory listing is a readdir or an ssh round trip. So
//! this is the fifth worker-backed store, after terminals, commands, diffs and
//! metrics, and it follows [`crate::kernel::diff`]'s shape exactly: **requests
//! are keyed and idempotent, work happens on a thread, and the result is
//! published.**
//!
//! Requests reach here from the flow through `store`, which the loop reads. A
//! plugin cannot call something that waits, and a command is an at-most-once
//! *act* — neither fits "tell me what is in this directory, and ask again as I
//! type".
//!
//! Two consequences a plugin renders rather than hides:
//!
//! - "not listed yet" is a state of its own, distinct from "nothing there" — a
//!   slow host must not look like an empty directory;
//! - a result is cached under the request that asked for it, so a listing
//!   arriving for a step the user has left is inert rather than needing to be
//!   dropped (v1 needed a generation stamp for this) — and a settled listing is
//!   re-read after a few seconds, because a directory created later must not be
//!   invisible for the life of the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::session::{HostDef, HostRegistry};

/// A repository the flow can offer, flattened for rendering.
///
/// One shape for every row, whatever produced it: a standalone bookmark, a
/// parent folder, a child scanned live under a local parent, or a child
/// persisted under a remote one. The flow builds headers and indentation from
/// `parent`/`is_parent` and never learns which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    /// Absolute path on the target host's filesystem.
    pub path: String,
    /// Display name — the final component, which is what a row leads with.
    pub name: String,
    /// The parent folder this row was imported under, if any.
    pub parent: Option<String>,
    /// Whether this row *is* a parent folder: a header, never a member.
    pub is_parent: bool,
    /// Whether the path is a git repo. `None` = never established, which stays
    /// selectable *and* worktree-capable — creation reports the truth.
    pub is_git: Option<bool>,
    /// A name to lead the row with instead of the path.
    ///
    /// `None` for anything the user bookmarked: they chose that path and it is
    /// how they think of the repository. Set for a row the flow *offers* on its
    /// own, where the path is an implementation detail the reader never typed —
    /// the interface directory is a long, install-specific path whose leaf
    /// (`ui`) says nothing about what picking it would do.
    pub label: Option<String>,
    /// Whether the flow offered this row itself rather than reading it from
    /// memory.
    ///
    /// An offered row is not a bookmark: it leads the list by construction, so
    /// its position says nothing about when it was last used. The flow needs
    /// the distinction because it finds a *just-added* path by recency — the
    /// first row of memory — and an offered row ahead of memory is picked up as
    /// that path instead.
    pub offered: bool,
}

/// One entry of the browse dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_git: bool,
}

/// What is known about a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Asked for, not answered. Distinct from `Ready` with nothing in it.
    Pending,
    Ready(Vec<BrowseEntry>),
    Failed(String),
}

/// A host key as the flow spells it: `""` for local, else a backend name
/// (`ssh:<name>` / `wsl:<name>`) — the same key `repo_bookmarks.host` uses.
pub type HostKey = String;

/// The keys a plugin asks through, in `store`.
pub const WANT_BOOKMARKS: &str = "want_bookmarks";
pub const WANT_BROWSE: &str = "want_browse";

/// What the flow is asking about right now.
///
/// Read off `store` each iteration. Absent means *not asking* —
/// which is why the local machine is `Some("")` rather than an empty string
/// standing in for both: a closed flow must cost nothing, and asking about local
/// must be expressible.
///
/// The two path-bearing wants are written as `"<host>\0<path>"`. A NUL cannot
/// occur in a path, so the split is unambiguous; the session list already keys
/// its repo groups the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wants {
    pub bookmarks: Option<HostKey>,
    pub browse: Option<(HostKey, String)>,
}

impl Wants {
    /// Build from the two raw store values, dropping one that is malformed
    /// rather than guessing what a plugin meant.
    pub fn new(bookmarks: Option<String>, browse: Option<String>) -> Self {
        Self {
            bookmarks,
            browse: browse.as_deref().and_then(split_want),
        }
    }
}

/// Split `"<host>\0<path>"`. An empty path is no request at all — that is what a
/// flow leaves behind while its input is still empty.
fn split_want(raw: &str) -> Option<(HostKey, String)> {
    let (host, path) = raw.split_once('\0')?;
    (!path.is_empty()).then(|| (host.to_string(), path.to_string()))
}

/// Cache key for anything asked about a path on a host.
type PathKey = (HostKey, String);

/// How long a directory listing is reused before it is read again.
///
/// A listing has to be cached — it is asked for on every keystroke — but caching
/// it for the life of the process means a directory created after the first look
/// never appears. Re-reading a settled listing after a few seconds bounds that
/// without making typing chatty. A *pending* one is never re-requested, however
/// long it takes: a slow ssh listing is still on its way.
const LISTING_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// A listing and when it arrived.
struct Listed {
    at: std::time::Instant,
    listing: Listing,
}

impl Listed {
    /// Whether this answer should be read again.
    ///
    /// A *pending* one never is, however old it has grown: a slow ssh listing is
    /// still on its way, and asking twice would spawn a second worker for it.
    fn stale(&self) -> bool {
        !matches!(self.listing, Listing::Pending) && self.at.elapsed() >= LISTING_TTL
    }
}

/// How long a host's remembered repositories are trusted.
///
/// The flow leaves `want_bookmarks` set for as long as it is open, so this is
/// asked for on every frame. Guarded only against a *concurrent* read, each answer
/// simply started the next one: a new SQLite connection and a live scan of every
/// parent folder's children, back to back, for the whole life of the flow.
const BOOKMARKS_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a **remote** folder bookmark's members are trusted before the host
/// is asked what is in it now.
///
/// A local folder is a `readdir`, so it is scanned on every bookmark read and
/// needs no interval of its own. A remote one is a connection per folder, which
/// is why it was originally scanned only at import — and why a folder on a host
/// stayed frozen at whatever it held that day. This is the compromise: long
/// enough that typing in the flow never pays for a scan, short enough that a
/// repository cloned on the host appears while you are still looking for it.
const REMOTE_RESCAN_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A host's remembered repositories and when they were read.
struct Remembered {
    at: std::time::Instant,
    rows: Vec<BookmarkRow>,
    /// Set when something rewrote this host's memory underneath these rows.
    ///
    /// They keep being published — a *background* refresh must never blank a
    /// list that is on screen, and dropping them does exactly that: the flow
    /// reads absent rows as `loading`, so a rescan finding one new repository
    /// would empty the picker and reset the cursor before the rows came back.
    /// The next request re-reads instead.
    superseded: bool,
}

impl Remembered {
    fn stale(&self) -> bool {
        self.superseded || self.at.elapsed() >= BOOKMARKS_TTL
    }
}

enum Done {
    Bookmarks {
        host: HostKey,
        rows: Vec<BookmarkRow>,
    },
    Listing {
        key: PathKey,
        listing: Listing,
    },
    /// A remote folder was scanned on its host. `rewrote` says whether what it
    /// found differed from what was remembered, and so whether the file
    /// underneath the rows has just changed.
    Rescan {
        key: PathKey,
        rewrote: bool,
    },
}

/// Serves the creation flow's four reads, caching each under its request.
pub struct RepoStore {
    hosts: HostRegistry,
    bookmarks: HashMap<HostKey, Remembered>,
    /// Hosts whose bookmark list is being (re)read, so asking every frame does
    /// not queue a thread per frame.
    bookmarks_inflight: std::collections::HashSet<HostKey>,
    listings: HashMap<PathKey, Listed>,
    /// When each remote folder bookmark was last scanned on its host, so
    /// [`REMOTE_RESCAN_TTL`] can decide whether to ask again.
    rescans: HashMap<PathKey, std::time::Instant>,
    /// Remote folders being scanned right now, so a bookmark read landing while
    /// one is in flight does not start a second connection for it.
    rescans_inflight: std::collections::HashSet<PathKey>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl RepoStore {
    /// A store that reads this machine only: the creation flow browses and
    /// remembers local folders, whichever Fleet host the session will run on,
    /// so no host key resolves to a connection.
    pub fn new() -> Self {
        Self::with_hosts(HostRegistry::default())
    }

    pub fn with_hosts(hosts: HostRegistry) -> Self {
        let (tx, rx) = channel();
        Self {
            hosts,
            bookmarks: HashMap::new(),
            bookmarks_inflight: std::collections::HashSet::new(),
            listings: HashMap::new(),
            rescans: HashMap::new(),
            rescans_inflight: std::collections::HashSet::new(),
            tx,
            rx,
        }
    }

    /// Resolve a flow host key to a host definition. `""` — and any name the
    /// registry does not know — is local.
    fn host_for(&self, key: &str) -> Option<&HostDef> {
        if key.is_empty() {
            return None;
        }
        self.hosts.resolve(key)
    }

    /// Whether any read is still on its way — a bookmark refresh or a pending
    /// listing.
    ///
    /// The animation clock asks: the creation flow draws a spinner over each of
    /// these, and a clock that moved only for in-flight *commands* would freeze
    /// them mid-spin — the flow is a pure pane, so its tree is reused until the
    /// epoch moves.
    ///
    /// A folder rescan is deliberately **not** one of them: the rows it might
    /// change are already on screen, so it is a refresh behind a complete
    /// answer, and spinning over them would say the list is not ready yet.
    pub fn in_flight(&self) -> bool {
        !self.bookmarks_inflight.is_empty()
            || self
                .listings
                .values()
                .any(|held| matches!(held.listing, Listing::Pending))
    }

    /// The rows for `host`, once they have been asked for.
    pub fn bookmarks(&self, host: &str) -> Option<&[BookmarkRow]> {
        self.bookmarks.get(host).map(|held| held.rows.as_slice())
    }

    /// What is known about `dir` on `host`.
    pub fn listing(&self, host: &str, dir: &str) -> Option<&Listing> {
        self.listings
            .get(&(host.to_string(), dir.to_string()))
            .map(|listed| &listed.listing)
    }

    /// Ask for a host's remembered repositories.
    ///
    /// Idempotent while one is in flight, but *not* cached forever: the flow
    /// asks again after a bookmark command lands, and re-reading a handful of
    /// rows is what makes an added path appear. The scan of a local parent's
    /// children is why this is a worker at all.
    pub fn request_bookmarks(&mut self, host: &str) {
        if self.bookmarks_inflight.contains(host) {
            return;
        }
        if self.bookmarks.get(host).is_some_and(|held| !held.stale()) {
            return;
        }
        self.bookmarks_inflight.insert(host.to_string());

        let host_key = host.to_string();
        let remote = self.host_for(host).cloned();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let rows = read_bookmarks(&host_key, remote.as_ref());
            let _ = tx.send(Done::Bookmarks {
                host: host_key,
                rows,
            });
        });
    }

    /// Re-read a host's bookmarks on the next request, after something wrote
    /// them.
    pub fn invalidate_bookmarks(&mut self) {
        self.bookmarks.clear();
    }

    /// Ask a **remote** host what each of its folder bookmarks holds now.
    ///
    /// Nothing to do for the local machine: [`scan_parents`] scans a local
    /// folder on every bookmark read, because a `readdir` costs nothing. A
    /// remote folder costs a connection, so it is asked on
    /// [`REMOTE_RESCAN_TTL`] and its answer is written back to the file —
    /// which is both what survives a restart and what a folder keeps showing
    /// while its host is unreachable.
    fn rescan_folders(&mut self, host: &str) {
        let Some(remote) = self.host_for(host).cloned() else {
            return;
        };
        let Some(held) = self.bookmarks.get(host) else {
            return;
        };
        // Collected before anything is spawned: the rows are borrowed from
        // `self`, and each request takes `&mut self`.
        let folders: Vec<(String, Vec<PathBuf>)> = held
            .rows
            .iter()
            .filter(|row| row.is_parent)
            .map(|folder| (folder.path.clone(), members_of(&held.rows, &folder.path)))
            .collect();

        for (folder, known) in folders {
            let key = (host.to_string(), folder);
            if self.rescans_inflight.contains(&key) {
                continue;
            }
            if self
                .rescans
                .get(&key)
                .is_some_and(|at| at.elapsed() < REMOTE_RESCAN_TTL)
            {
                continue;
            }
            self.rescans_inflight.insert(key.clone());
            let remote = remote.clone();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let rewrote = rescan_folder(&remote, &key.0, &key.1, known);
                let _ = tx.send(Done::Rescan { key, rewrote });
            });
        }
    }

    /// Ask what is in `dir` on `host`, unless a fresh answer is already held or
    /// one is on its way.
    pub fn request_listing(&mut self, host: &str, dir: &str) {
        let key = (host.to_string(), dir.to_string());
        if self.listings.get(&key).is_some_and(|held| !held.stale()) {
            return;
        }
        self.remember_listing(key.clone(), Listing::Pending);

        let remote = self.host_for(host).cloned();
        let dir = dir.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let listing = match crate::git::list_dir_entries_on(remote.as_ref(), &dir) {
                Ok(crate::git::DirListing::Missing) => {
                    Listing::Failed(format!("No such directory: {dir}"))
                }
                Ok(crate::git::DirListing::Entries(entries)) => Listing::Ready(
                    entries
                        .into_iter()
                        .map(|(name, is_git)| BrowseEntry { name, is_git })
                        .collect(),
                ),
                Err(e) => Listing::Failed(format!("{e:#}")),
            };
            let _ = tx.send(Done::Listing { key, listing });
        });
    }

    /// Hold a listing, stamped so [`LISTING_TTL`] can retire it.
    fn remember_listing(&mut self, key: PathKey, listing: Listing) {
        self.listings.insert(
            key,
            Listed {
                at: std::time::Instant::now(),
                listing,
            },
        );
    }

    /// Fold finished work in. True when anything arrived, so the loop repaints.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.rx.try_recv() {
            match done {
                Done::Bookmarks { host, rows } => {
                    self.bookmarks_inflight.remove(&host);
                    self.bookmarks.insert(
                        host.clone(),
                        Remembered {
                            at: std::time::Instant::now(),
                            rows,
                            superseded: false,
                        },
                    );
                    // The rows name the folders, so this is the first moment a
                    // scan of them can be asked for.
                    self.rescan_folders(&host);
                }
                Done::Listing { key, listing } => {
                    self.remember_listing(key, listing);
                }
                Done::Rescan { key, rewrote } => {
                    self.rescans_inflight.remove(&key);
                    self.rescans.insert(key.clone(), std::time::Instant::now());
                    if rewrote {
                        // The memory this host's rows were built from has just
                        // been rewritten underneath them, so they are the
                        // previous answer; the next request re-reads. Only this
                        // host's — another one's rows are untouched and
                        // re-reading them would be a query for nothing.
                        if let Some(held) = self.bookmarks.get_mut(&key.0) {
                            held.superseded = true;
                        }
                    }
                    // Deliberately NOT a change: nothing a reader can see has
                    // moved yet — the rows on screen are still the rows on
                    // screen, and the re-read reports its own arrival. Saying
                    // otherwise would advance the data epoch every interval per
                    // remote folder, rebuilding every published group and
                    // dropping every pure pane's cached tree for a scan that
                    // usually finds nothing.
                    continue;
                }
            }
            changed = true;
        }
        changed
    }

    /// Ask for everything the flow is currently asking about.
    ///
    /// One call from the loop per iteration. Every request underneath is
    /// idempotent, so this costs two hash lookups once the answers are in.
    pub fn serve(&mut self, wants: &Wants) {
        // Cloned rather than borrowed: each request takes `&mut self`, and the
        // wants are the loop's, not the store's.
        let wants = wants.clone();
        if let Some(host) = wants.bookmarks {
            self.request_bookmarks(&host);
        }
        if let Some((host, dir)) = wants.browse {
            self.request_listing(&host, &dir);
        }
    }

    /// Seed rows directly, so a test can render a flow without the file.
    #[doc(hidden)]
    pub fn set_bookmarks_for_test(&mut self, host: &str, rows: Vec<BookmarkRow>) {
        self.bookmarks.insert(
            host.to_string(),
            Remembered {
                at: std::time::Instant::now(),
                rows,
                superseded: false,
            },
        );
    }

    /// Seed a listing directly, so a test can render the dropdown without a
    /// filesystem.
    #[doc(hidden)]
    pub fn set_listing_for_test(&mut self, host: &str, dir: &str, listing: Listing) {
        self.remember_listing((host.to_string(), dir.to_string()), listing);
    }
}

impl Default for RepoStore {
    fn default() -> Self {
        Self::new()
    }
}

use super::bookmarks::Bookmark;

/// Read a host's bookmarks and flatten them into rows. Called on a worker.
///
/// v1's `rebuild_repo_picker_rows`. Its asymmetry survives, but only as *when* a
/// folder is scanned, never whether: a **local** folder is scanned right here,
/// every time, because a `readdir` is free; a **remote** one is scanned by
/// [`RepoStore::rescan_folders`] on [`REMOTE_RESCAN_TTL`] and persisted, so by
/// the time it reaches here it is an ordinary remembered member. Either way the
/// rows are what the folder holds now rather than what it held at import, and
/// either way they arrive as the same row shape.
fn read_bookmarks(host: &str, remote: Option<&HostDef>) -> Vec<BookmarkRow> {
    let bookmarks = super::bookmarks::list(host);
    // Local only. A remote folder is scanned on its own interval by
    // `rescan_folders`, which writes what it finds back to the file — so by
    // the time it reaches here it is already a persisted member.
    let scanned = match remote {
        None => scan_parents(&bookmarks),
        Some(_) => HashMap::new(),
    };
    let mut rows = flatten(&bookmarks, &scanned);
    if remote.is_none() {
        offer_interface_dir(&mut rows);
    }
    rows
}

/// Put the interface directory at the top of the local repository list.
///
/// Editing a pane is the one piece of work every thurbox user can do without
/// cloning anything, and it was the one directory the list never offered — you
/// had to know where the interface lives and type the path. Offered rather than
/// persisted: it is wherever `bundled::resolve` says it is *now*, which changes
/// with the working directory (a `./ui` beside it wins), so a remembered path
/// would go stale the moment you started thurbox somewhere else.
///
/// Local only. The interface runs on this machine, so its directory is not a
/// path on a host — the same reason a local home is not offered as a remote
/// repository.
///
/// A user who bookmarked it themselves keeps their row: theirs may sit under a
/// folder, and moving it would rearrange a list they arranged.
fn offer_interface_dir(rows: &mut Vec<BookmarkRow>) {
    // `resolve(false)`: reading the list must not materialize an interface, or
    // opening the creation flow would write files as a side effect.
    let Ok((dir, _chosen, _report)) = super::bundled::resolve(false) else {
        return;
    };
    if !dir.is_dir() || rows.iter().any(|row| row.path == dir.to_string_lossy()) {
        return;
    }
    // `is_git` is probed like any other row: the interface directory is not a
    // repository under a default install, and the flow needs to know — a
    // worktree cannot be cut from something that is not one.
    let is_git = Some(dir.join(".git").exists());
    let mut offered = row(&dir, None, false, is_git);
    // Named for what picking it does, not for where it happens to live. Its leaf
    // is usually `ui`, which tells a reader nothing, and the rest of the path is
    // install-specific boilerplate that the row was truncating anyway.
    offered.label = Some("Thurbox interface — edit your panes".to_string());
    // Flagged so a reader of the list can tell placement from recency: this row
    // is first because this function puts it here, not because it was just used.
    offered.offered = true;
    rows.insert(0, offered);
}

/// Turn persisted bookmarks into the flat row list a flow renders.
///
/// Separated from the read so the shape — headers, their members, and what is
/// suppressed as already covered — is testable without the file.
fn flatten(bookmarks: &[Bookmark], scanned: &HashMap<PathBuf, Vec<PathBuf>>) -> Vec<BookmarkRow> {
    let persisted = group_persisted(bookmarks);

    // A path a parent already covers is emitted under that parent, not twice.
    let covered: std::collections::HashSet<&PathBuf> = scanned
        .values()
        .flatten()
        .chain(persisted.values().flatten().map(|child| &child.repo_path))
        .collect();

    let mut rows = Vec::new();
    // Guards against any path appearing twice: duplicate bookmarks, a member
    // shared by two folders, a folder nested inside another.
    let mut emitted: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for bookmark in bookmarks {
        if bookmark.is_parent {
            push_folder(&mut rows, &mut emitted, bookmark, scanned, &persisted);
        } else if !covered.contains(&bookmark.repo_path) {
            push_standalone(&mut rows, &mut emitted, bookmark);
        }
    }
    rows
}

/// One repository not covered by any folder above it.
fn push_standalone(
    rows: &mut Vec<BookmarkRow>,
    emitted: &mut std::collections::HashSet<PathBuf>,
    bookmark: &Bookmark,
) {
    if !emitted.insert(bookmark.repo_path.clone()) {
        return;
    }
    rows.push(row(&bookmark.repo_path, None, false, bookmark.is_git));
}

/// A folder header followed by its members.
fn push_folder(
    rows: &mut Vec<BookmarkRow>,
    emitted: &mut std::collections::HashSet<PathBuf>,
    bookmark: &Bookmark,
    scanned: &HashMap<PathBuf, Vec<PathBuf>>,
    persisted: &HashMap<&PathBuf, Vec<&Bookmark>>,
) {
    if !emitted.insert(bookmark.repo_path.clone()) {
        return;
    }
    rows.push(row(&bookmark.repo_path, None, true, None));
    let parent = display(&bookmark.repo_path);
    // A scan is the truth about a folder, and what was written at import time is
    // only the last scan that succeeded: chaining the two would keep offering a
    // repository that has since been deleted, which is what it used to do.
    // Scanned members are repositories by construction; persisted ones carry
    // whatever was established when they were imported.
    let members: Vec<(&PathBuf, Option<bool>)> = match scanned.get(&bookmark.repo_path) {
        Some(children) => children.iter().map(|path| (path, Some(true))).collect(),
        None => persisted
            .get(&bookmark.repo_path)
            .into_iter()
            .flatten()
            .map(|child| (&child.repo_path, child.is_git))
            .collect(),
    };
    for (path, is_git) in members {
        if emitted.insert(path.clone()) {
            rows.push(row(path, Some(parent.clone()), false, is_git));
        }
    }
}

/// The git repositories directly under each folder bookmark, scanned now.
///
/// A folder that cannot be **read** — deleted, permission denied, a path that is
/// not a directory — is left out rather than reported empty, and the difference
/// is the whole contract of the map: a key means a scan succeeded and is
/// authoritative, its absence means there is no scan and what was persisted
/// stands in.
///
/// A folder that reads as empty is empty, and that is deliberate — it is the
/// same answer as "you deleted the last repository in it", which is half of what
/// a rescan is for. The consequence is worth knowing: a mount point whose drive
/// is not mounted is usually a readable empty directory, so a folder imported
/// from one goes empty until it is mounted again, rather than holding its last
/// contents. Nothing in a directory listing distinguishes the two, and guessing
/// from "it used to have members" would keep a folder you really did empty.
fn scan_parents(bookmarks: &[Bookmark]) -> HashMap<PathBuf, Vec<PathBuf>> {
    bookmarks
        .iter()
        .filter(|bookmark| bookmark.is_parent)
        // `read_dir` rather than `is_dir`, because the question is whether the
        // scan can be *trusted*: `scan_child_repos` answers a directory it
        // cannot open with an empty list, which is indistinguishable from a
        // folder holding nothing.
        .filter(|bookmark| std::fs::read_dir(&bookmark.repo_path).is_ok())
        .map(|bookmark| {
            (
                bookmark.repo_path.clone(),
                crate::git::scan_child_repos(&bookmark.repo_path),
            )
        })
        .collect()
}

/// The members a row list already offers under `folder`, sorted — what a rescan
/// compares its answer against to decide whether anything has changed.
fn members_of(rows: &[BookmarkRow], folder: &str) -> Vec<PathBuf> {
    let mut known: Vec<PathBuf> = rows
        .iter()
        .filter(|row| row.parent.as_deref() == Some(folder))
        .map(|row| PathBuf::from(&row.path))
        .collect();
    known.sort();
    known
}

/// Scan one remote folder on its host and, when what it holds has changed, write
/// the members back. Called on a worker; true when the file was rewritten.
///
/// A failed scan changes **nothing**. An unreachable host, a folder the user has
/// not mounted yet, a connection that timed out — each must leave the folder
/// offering what it last held rather than emptying it, which is also what makes
/// the rows usable with no network at all.
fn rescan_folder(host: &HostDef, host_key: &str, folder: &str, known: Vec<PathBuf>) -> bool {
    let children = match crate::git::scan_child_repos_on(host, folder) {
        Ok(children) => children,
        Err(e) => {
            tracing::warn!("could not rescan {folder} on {}: {e:#}", host.name);
            return false;
        }
    };
    // Both sides are sorted absolute paths, so this is the whole question — and
    // asking it is what keeps a folder that has not changed from invalidating
    // the rows, and repainting the flow, every half minute.
    if children == known {
        return false;
    }
    if let Err(e) = super::bookmarks::replace_members(host_key, Path::new(folder), &children) {
        tracing::warn!("could not record the rescan of {folder}: {e}");
        return false;
    }
    true
}

/// Members grouped under the folder they were imported into. A member whose
/// folder bookmark is gone is left out, so it falls back to standing alone.
fn group_persisted(bookmarks: &[Bookmark]) -> HashMap<&PathBuf, Vec<&Bookmark>> {
    let folders: std::collections::HashSet<&PathBuf> = bookmarks
        .iter()
        .filter(|bookmark| bookmark.is_parent)
        .map(|bookmark| &bookmark.repo_path)
        .collect();
    let mut grouped: HashMap<&PathBuf, Vec<&Bookmark>> = HashMap::new();
    for bookmark in bookmarks {
        if let Some(folder) = bookmark
            .parent_path
            .as_ref()
            .filter(|path| folders.contains(path))
        {
            grouped.entry(folder).or_default().push(bookmark);
        }
    }
    grouped
}

fn display(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn row(path: &Path, parent: Option<String>, is_parent: bool, is_git: Option<bool>) -> BookmarkRow {
    BookmarkRow {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| display(path)),
        path: display(path),
        parent,
        is_parent,
        is_git,
        label: None,
        offered: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_known_before_it_is_asked_for() {
        let store = RepoStore::with_hosts(HostRegistry::default());
        assert!(store.bookmarks("").is_none());
        assert!(store.listing("", "/tmp").is_none());
    }

    #[test]
    fn requesting_a_listing_returns_at_once_and_reads_pending() {
        // The property that matters: asking does not wait for the filesystem.
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        let started = std::time::Instant::now();
        store.request_listing("", "/definitely/not/here");
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert_eq!(
            store.listing("", "/definitely/not/here"),
            Some(&Listing::Pending)
        );
    }

    #[test]
    fn pending_is_distinct_from_an_empty_directory() {
        // A slow host must not read as a directory with nothing in it.
        assert_ne!(Listing::Pending, Listing::Ready(Vec::new()));
    }

    #[test]
    fn a_second_request_does_not_relist() {
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.request_listing("", "/tmp");
        store.set_listing_for_test("", "/tmp", Listing::Ready(Vec::new()));
        store.request_listing("", "/tmp");
        assert_eq!(store.listing("", "/tmp"), Some(&Listing::Ready(Vec::new())));
    }

    /// A held answer, back-dated so the age rule can be exercised without
    /// waiting out the interval.
    fn held(listing: Listing, age: std::time::Duration) -> Listed {
        Listed {
            at: std::time::Instant::now() - age,
            listing,
        }
    }

    #[test]
    fn a_settled_listing_is_read_again_once_it_is_old() {
        // Without this a directory created after the first look never appears,
        // because the answer is cached for the life of the process.
        assert!(!held(Listing::Ready(Vec::new()), std::time::Duration::ZERO).stale());
        assert!(held(Listing::Ready(Vec::new()), LISTING_TTL).stale());
        assert!(held(Listing::Failed("gone".into()), LISTING_TTL).stale());
    }

    #[test]
    fn a_pending_listing_is_never_asked_for_twice() {
        // However slow the host is: a second request would spawn a second worker
        // for an answer already on its way.
        assert!(!held(Listing::Pending, LISTING_TTL * 10).stale());
    }

    #[test]
    fn an_old_listing_is_replaced_by_a_fresh_request() {
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.set_listing_for_test("", "/tmp", Listing::Ready(Vec::new()));
        store.request_listing("", "/tmp");
        assert_eq!(
            store.listing("", "/tmp"),
            Some(&Listing::Ready(Vec::new())),
            "a fresh answer is reused"
        );

        store.listings.insert(
            (String::new(), "/tmp".into()),
            held(Listing::Ready(Vec::new()), LISTING_TTL),
        );
        store.request_listing("", "/tmp");
        assert_eq!(
            store.listing("", "/tmp"),
            Some(&Listing::Pending),
            "a stale one is asked about again"
        );
    }

    #[test]
    fn a_missing_directory_reports_rather_than_hanging() {
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.request_listing("", "/definitely/not/here");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            store.poll();
            if let Some(Listing::Failed(_)) = store.listing("", "/definitely/not/here") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the failure never arrived");
    }

    #[test]
    fn listings_are_keyed_by_host_as_well_as_directory() {
        // Two machines' `/srv` are different directories, and a result for one
        // must never be shown for the other.
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.set_listing_for_test("", "/srv", Listing::Ready(Vec::new()));
        store.set_listing_for_test(
            "ssh:box",
            "/srv",
            Listing::Ready(vec![BrowseEntry {
                name: "repo".to_string(),
                is_git: true,
            }]),
        );
        assert_eq!(store.listing("", "/srv"), Some(&Listing::Ready(Vec::new())));
        assert!(matches!(
            store.listing("ssh:box", "/srv"),
            Some(Listing::Ready(entries)) if entries.len() == 1
        ));
    }

    #[test]
    fn a_bookmark_request_is_not_repeated_while_in_flight() {
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.request_bookmarks("");
        assert!(store.bookmarks_inflight.contains(""));
        store.request_bookmarks("");
        assert_eq!(store.bookmarks_inflight.len(), 1);
    }

    #[test]
    fn a_rescan_that_finds_nothing_does_not_move_the_data_epoch() {
        // The loop repaints on `poll`, and a repaint advances the epoch: every
        // published group is rebuilt and every pure pane's cached tree dropped.
        // A folder that has not changed must not cost that every interval.
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store
            .tx
            .send(Done::Rescan {
                key: ("ssh:box".into(), "/srv".into()),
                rewrote: false,
            })
            .expect("send");
        assert!(!store.poll(), "nothing a reader can see has moved");
    }

    #[test]
    fn a_rescan_that_found_something_leaves_the_rows_on_screen() {
        // Dropping them would blank the picker: the flow reads absent rows as
        // `loading`, so a background scan finding one new repository would empty
        // a list the user is looking at until the re-read landed.
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.set_bookmarks_for_test(
            "ssh:box",
            vec![row(Path::new("/srv/one"), None, false, None)],
        );
        store
            .tx
            .send(Done::Rescan {
                key: ("ssh:box".into(), "/srv".into()),
                rewrote: true,
            })
            .expect("send");
        store.poll();

        let held = store.bookmarks.get("ssh:box").expect("the rows are kept");
        assert_eq!(held.rows.len(), 1, "and are still published");
        assert!(held.stale(), "but the next request re-reads them");
    }

    #[test]
    fn a_folder_is_not_rescanned_again_while_one_is_in_flight() {
        // Bookmarks are re-read every BOOKMARKS_TTL while the flow is open, and
        // each read asks for a rescan; without the guard a slow ssh host would
        // collect a connection per read.
        let mut store = RepoStore::with_hosts(HostRegistry {
            hosts: vec![HostDef {
                name: "box".into(),
                destination: "nowhere.invalid".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let folder = row(Path::new("/srv"), None, true, None);
        store.set_bookmarks_for_test("ssh:box", vec![folder]);
        store.rescan_folders("ssh:box");
        assert_eq!(store.rescans_inflight.len(), 1);
        store.rescan_folders("ssh:box");
        assert_eq!(store.rescans_inflight.len(), 1, "not one per read");
    }

    #[test]
    fn a_local_folder_is_never_rescanned_over_a_transport() {
        // It has no host to ask: `read_bookmarks` scans it on every read.
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.set_bookmarks_for_test("", vec![row(Path::new("/src"), None, true, None)]);
        store.rescan_folders("");
        assert!(store.rescans_inflight.is_empty());
    }

    #[test]
    fn invalidating_bookmarks_drops_what_was_read() {
        let mut store = RepoStore::with_hosts(HostRegistry::default());
        store.set_bookmarks_for_test(
            "",
            vec![row(Path::new("/src/thing"), None, false, Some(true))],
        );
        store.invalidate_bookmarks();
        assert!(store.bookmarks("").is_none());
    }

    /// A persisted bookmark, with the fields these tests care about.
    fn saved(path: &str, is_parent: bool, parent: Option<&str>) -> Bookmark {
        Bookmark {
            host: String::new(),
            repo_path: PathBuf::from(path),
            last_used_at: 0,
            is_parent,
            is_git: Some(true),
            parent_path: parent.map(PathBuf::from),
        }
    }

    /// No folder was scanned — the shape a remote host's rows are built in, and
    /// a local one's whenever the folder could not be read.
    fn no_scan() -> HashMap<PathBuf, Vec<PathBuf>> {
        HashMap::new()
    }

    /// A folder scanned successfully, holding exactly `children`.
    fn scanned(folder: &str, children: &[&str]) -> HashMap<PathBuf, Vec<PathBuf>> {
        HashMap::from([(
            PathBuf::from(folder),
            children.iter().map(PathBuf::from).collect(),
        )])
    }

    /// `(path, parent, is_parent)` per row, which is the shape a flow renders
    /// headers and indentation from.
    fn shape(rows: &[BookmarkRow]) -> Vec<(&str, Option<&str>, bool)> {
        rows.iter()
            .map(|row| (row.path.as_str(), row.parent.as_deref(), row.is_parent))
            .collect()
    }

    #[test]
    fn a_standalone_bookmark_is_one_row() {
        let rows = flatten(&[saved("/src/thing", false, None)], &no_scan());
        assert_eq!(shape(&rows), [("/src/thing", None, false)]);
    }

    #[test]
    fn a_folder_leads_its_members() {
        // The header first, then what was imported under it — the order the flow
        // draws them in, since a member is indented beneath its header.
        let rows = flatten(
            &[
                saved("/src", true, None),
                saved("/src/one", false, Some("/src")),
                saved("/src/two", false, Some("/src")),
            ],
            // Nothing scanned: the members are what was persisted at import time,
            // which is what a remote folder shows until its next rescan lands.
            &no_scan(),
        );
        assert_eq!(
            shape(&rows),
            [
                ("/src", None, true),
                ("/src/one", Some("/src"), false),
                ("/src/two", Some("/src"), false),
            ]
        );
    }

    #[test]
    fn a_scan_replaces_what_the_import_remembered() {
        // The rule the whole rescan rests on. `two` was cloned after the import
        // and `gone` has been deleted since; a folder that chained the scan onto
        // its memory offered all three.
        let rows = flatten(
            &[
                saved("/src", true, None),
                saved("/src/one", false, Some("/src")),
                saved("/src/gone", false, Some("/src")),
            ],
            &scanned("/src", &["/src/one", "/src/two"]),
        );
        assert_eq!(
            shape(&rows),
            [
                ("/src", None, true),
                ("/src/one", Some("/src"), false),
                ("/src/two", Some("/src"), false),
            ]
        );
    }

    #[test]
    fn a_deleted_member_does_not_come_back_as_a_row_of_its_own() {
        // The other half: a member the scan dropped is still *covered* by its
        // folder, so it does not fall out of the group and stand alone — which
        // would be the same wrong row in a different place.
        let rows = flatten(
            &[
                saved("/src", true, None),
                saved("/src/gone", false, Some("/src")),
            ],
            &scanned("/src", &[]),
        );
        assert_eq!(shape(&rows), [("/src", None, true)]);
    }

    #[test]
    fn a_folder_that_cannot_be_read_keeps_what_it_last_held() {
        // A local folder on an unmounted drive: `scan_parents` leaves it out of
        // the map entirely rather than reporting it empty, and memory stands in.
        let bookmarks = [
            saved("/mnt/src", true, None),
            saved("/mnt/src/one", false, Some("/mnt/src")),
        ];
        assert!(
            scan_parents(&bookmarks).is_empty(),
            "a folder that is not there is not a scan of nothing"
        );
        assert_eq!(
            shape(&flatten(&bookmarks, &scan_parents(&bookmarks))),
            [
                ("/mnt/src", None, true),
                ("/mnt/src/one", Some("/mnt/src"), false)
            ]
        );
    }

    #[test]
    fn a_rescan_compares_against_the_members_already_on_offer() {
        // Sorted, because the scan is: the comparison is what stops an unchanged
        // folder rewriting the file and repainting the flow every interval.
        let rows = flatten(
            &[
                saved("/src", true, None),
                saved("/src/two", false, Some("/src")),
                saved("/src/one", false, Some("/src")),
            ],
            &no_scan(),
        );
        assert_eq!(
            members_of(&rows, "/src"),
            [PathBuf::from("/src/one"), PathBuf::from("/src/two")]
        );
        assert!(
            members_of(&rows, "/elsewhere").is_empty(),
            "and it answers about one folder, not every row that has a parent"
        );
    }

    #[test]
    fn a_repository_a_folder_covers_is_not_listed_twice() {
        // Adding a path by hand and then importing the folder above it must not
        // produce two rows for the same repository.
        let rows = flatten(
            &[
                saved("/src/one", false, None),
                saved("/src", true, None),
                saved("/src/one", false, Some("/src")),
            ],
            &no_scan(),
        );
        assert_eq!(
            shape(&rows),
            [("/src", None, true), ("/src/one", Some("/src"), false)],
            "the standalone row gives way to the one under the folder"
        );
    }

    #[test]
    fn a_member_whose_folder_is_gone_stands_alone() {
        // Forgetting a folder should take its members with it, but a row left
        // behind by an older version — or a half-finished delete — must still be
        // offered rather than silently dropped.
        let rows = flatten(&[saved("/src/one", false, Some("/src"))], &no_scan());
        assert_eq!(shape(&rows), [("/src/one", None, false)]);
    }

    #[test]
    fn a_duplicate_path_is_emitted_once() {
        let rows = flatten(
            &[
                saved("/src/one", false, None),
                saved("/src/one", false, None),
            ],
            &no_scan(),
        );
        assert_eq!(shape(&rows), [("/src/one", None, false)]);
    }

    #[test]
    fn a_folder_row_is_never_offered_as_a_repository() {
        // It is a header: selecting it would create a session against a directory
        // of repositories rather than a repository.
        let rows = flatten(&[saved("/src", true, None)], &no_scan());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_parent);
        assert_eq!(
            rows[0].is_git, None,
            "and its git-ness is not claimed either way"
        );
    }

    #[test]
    fn a_row_leads_with_its_final_component() {
        let built = row(Path::new("/home/me/src/thurbox"), None, false, Some(true));
        assert_eq!(built.name, "thurbox");
        assert_eq!(built.path, "/home/me/src/thurbox");
    }

    #[test]
    fn a_want_carries_a_host_and_a_path() {
        let wants = Wants::new(Some(String::new()), Some("ssh:box\0/srv/repos".into()));
        // The local machine is an empty host, which is not the same as no
        // request at all.
        assert_eq!(wants.bookmarks.as_deref(), Some(""));
        assert_eq!(wants.browse, Some(("ssh:box".into(), "/srv/repos".into())));
    }

    #[test]
    fn a_malformed_or_empty_want_asks_for_nothing() {
        // No separator at all, and a separator with nothing after it — the
        // second is what a flow leaves while its input is still empty.
        assert_eq!(Wants::new(None, Some("/srv/repos".into())).browse, None);
        let wants = Wants::new(None, Some("ssh:box\0".into()));
        assert_eq!(wants.bookmarks, None);
        assert_eq!(wants.browse, None);
    }

    #[test]
    fn an_empty_host_key_is_local() {
        let store = RepoStore::with_hosts(HostRegistry::default());
        assert!(store.host_for("").is_none());
        // And an unknown one, rather than being a hard error, is local too: the
        // flow must not become unusable because `hosts.toml` changed under it.
        assert!(store.host_for("ssh:gone").is_none());
    }

    #[test]
    fn the_interface_directory_is_offered_first() {
        // Editing a pane is work every user can do without cloning anything, and
        // it was the one directory the list never offered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let ui = home.path().join("ui");
        std::fs::create_dir_all(ui.join("plugins")).expect("mkdir");
        std::env::set_var("THURBOX_UI_DIR", &ui);

        let mut rows = Vec::new();
        offer_interface_dir(&mut rows);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].path, ui.to_string_lossy());
        assert!(!rows[0].is_parent, "it is a repository row, not a folder");
        assert_eq!(
            rows[0].is_git,
            Some(false),
            "a default install is not a repository, and the flow has to know"
        );
        // The path is a long install-specific string whose leaf is `ui`, which
        // says nothing about what picking the row would do. A row the flow offers
        // on its own account names itself.
        let label = rows[0].label.as_deref().expect("the offered row is named");
        assert!(
            label.contains("interface"),
            "the name says what it is: {label}"
        );
        // The flag is what keeps it out of the flow's recency lookup: being
        // first here is placement, not a bookmark that was just used.
        assert!(rows[0].offered, "an offered row says so");
        std::env::remove_var("THURBOX_UI_DIR");
    }

    #[test]
    fn a_bookmark_of_your_own_keeps_its_place() {
        // Yours may sit under a folder you arranged; inserting a second row for
        // the same path would rearrange a list you built.
        let home = tempfile::TempDir::new().expect("tempdir");
        let ui = home.path().join("ui");
        std::fs::create_dir_all(&ui).expect("mkdir");
        std::env::set_var("THURBOX_UI_DIR", &ui);

        let mut rows = vec![row(&ui, Some("projects".into()), false, Some(true))];
        offer_interface_dir(&mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].parent.as_deref(), Some("projects"));
        std::env::remove_var("THURBOX_UI_DIR");
    }
}
