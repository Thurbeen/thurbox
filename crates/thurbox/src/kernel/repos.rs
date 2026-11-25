use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    pub path: String,
    pub name: String,
    pub parent: Option<String>,
    pub is_parent: bool,
    pub is_git: Option<bool>,
    pub label: Option<String>,
    pub offered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_git: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Pending,
    Ready(Vec<BrowseEntry>),
    Failed(String),
}

pub type HostKey = String;

pub const WANT_BOOKMARKS: &str = "want_bookmarks";
pub const WANT_BROWSE: &str = "want_browse";
pub const WANT_BRANCHES: &str = "want_branches";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wants {
    pub bookmarks: Option<HostKey>,
    pub browse: Option<(HostKey, String)>,
    pub branches: Option<String>,
}

impl Wants {
    pub fn new(bookmarks: Option<String>, browse: Option<String>) -> Self {
        Self {
            bookmarks,
            browse: browse.as_deref().and_then(split_want),
            branches: None,
        }
    }

    pub fn with_branches(mut self, repo: Option<String>) -> Self {
        self.branches = repo.filter(|repo| !repo.is_empty());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchListing {
    Pending,
    NotGit,
    Failed(String),
    Ready(crate::git::Branches),
}

fn split_want(raw: &str) -> Option<(HostKey, String)> {
    let (host, path) = raw.split_once('\0')?;
    (!path.is_empty()).then(|| (host.to_string(), path.to_string()))
}

type PathKey = (HostKey, String);

const LISTING_TTL: std::time::Duration = std::time::Duration::from_secs(5);

struct Listed {
    at: std::time::Instant,
    listing: Listing,
}

impl Listed {
    fn stale(&self) -> bool {
        !matches!(self.listing, Listing::Pending) && self.at.elapsed() >= LISTING_TTL
    }
}

const BOOKMARKS_TTL: std::time::Duration = std::time::Duration::from_secs(5);

struct Remembered {
    at: std::time::Instant,
    rows: Vec<BookmarkRow>,
}

impl Remembered {
    fn stale(&self) -> bool {
        self.at.elapsed() >= BOOKMARKS_TTL
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
        dir: Option<String>,
    },
    Branches {
        repo: String,
        listing: BranchListing,
        fetched: Option<Option<String>>,
    },
}

const FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

struct BranchesHeld {
    at: std::time::Instant,
    listing: BranchListing,
}

impl BranchesHeld {
    fn stale(&self) -> bool {
        self.at.elapsed() >= LISTING_TTL
    }
}

pub struct RepoStore {
    bookmarks: HashMap<HostKey, Remembered>,
    bookmarks_inflight: std::collections::HashSet<HostKey>,
    listings: HashMap<PathKey, Listed>,
    branches: HashMap<String, BranchesHeld>,
    branches_inflight: std::collections::HashSet<String>,
    fetched: std::collections::HashSet<String>,
    fetch_notes: HashMap<String, String>,
    fleets: Vec<crate::fleet_link::FleetLink>,
    resolved: HashMap<PathKey, String>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl RepoStore {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            bookmarks: HashMap::new(),
            bookmarks_inflight: std::collections::HashSet::new(),
            listings: HashMap::new(),
            branches: HashMap::new(),
            branches_inflight: std::collections::HashSet::new(),
            fetched: std::collections::HashSet::new(),
            fetch_notes: HashMap::new(),
            fleets: Vec::new(),
            resolved: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn with_fleets(fleets: &[crate::fleet_link::FleetLink]) -> Self {
        Self {
            fleets: fleets.to_vec(),
            ..Self::new()
        }
    }

    fn remote_place(&self, host: &str) -> Option<(crate::fleet_link::FleetLink, String)> {
        let (fleet, place) = host.split_once('/')?;
        let link = self.fleets.iter().find(|link| link.name() == fleet)?;
        if link.is_local() && place == "local" {
            return None;
        }
        Some((link.clone(), place.to_string()))
    }

    pub fn in_flight(&self) -> bool {
        !self.bookmarks_inflight.is_empty()
            || self
                .listings
                .values()
                .any(|held| matches!(held.listing, Listing::Pending))
            || self
                .branches
                .values()
                .any(|held| matches!(held.listing, BranchListing::Pending))
    }

    pub fn branches(&self, repo: &str) -> Option<&BranchListing> {
        self.branches.get(repo).map(|held| &held.listing)
    }

    pub fn fetch_note(&self, repo: &str) -> Option<&str> {
        self.fetch_notes.get(repo).map(String::as_str)
    }

    pub fn request_branches(&mut self, repo: &str) {
        if self.branches_inflight.contains(repo) {
            return;
        }
        if let Some((link, place, path)) = repo
            .split_once('\0')
            .and_then(|(host, path)| self.remote_place(host).map(|(l, p)| (l, p, path)))
        {
            if self.branches.get(repo).is_some_and(|held| !held.stale()) {
                return;
            }
            self.branches_inflight.insert(repo.to_string());
            self.branches
                .entry(repo.to_string())
                .or_insert_with(|| BranchesHeld {
                    at: std::time::Instant::now(),
                    listing: BranchListing::Pending,
                });
            let (repo, path) = (repo.to_string(), path.to_string());
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let listing = remote_branches(&link, &place, &path);
                let _ = tx.send(Done::Branches {
                    repo,
                    listing,
                    fetched: None,
                });
            });
            return;
        }
        let fetch = !self.fetched.contains(repo);
        if !fetch && self.branches.get(repo).is_some_and(|held| !held.stale()) {
            return;
        }
        self.fetched.insert(repo.to_string());
        self.branches_inflight.insert(repo.to_string());
        if fetch || !self.branches.contains_key(repo) {
            self.branches.insert(
                repo.to_string(),
                BranchesHeld {
                    at: std::time::Instant::now(),
                    listing: BranchListing::Pending,
                },
            );
        }
        let repo = repo.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let fetched = fetch.then(|| crate::git::fetch_origin(&repo, FETCH_DEADLINE).err());
            let listing = match crate::git::branches(&repo) {
                Ok(found) => BranchListing::Ready(found),
                Err(crate::git::BranchesUnread::NotGit) => BranchListing::NotGit,
                Err(crate::git::BranchesUnread::Failed(why)) => BranchListing::Failed(why),
            };
            let _ = tx.send(Done::Branches {
                repo,
                listing,
                fetched,
            });
        });
    }

    pub fn bookmarks(&self, host: &str) -> Option<&[BookmarkRow]> {
        self.bookmarks.get(host).map(|held| held.rows.as_slice())
    }

    pub fn resolved_dir(&self, host: &str, dir: &str) -> Option<&str> {
        self.resolved
            .get(&(host.to_string(), dir.to_string()))
            .map(String::as_str)
    }

    #[doc(hidden)]
    pub fn set_resolved_dir_for_test(&mut self, host: &str, dir: &str, resolved: &str) {
        self.resolved
            .insert((host.to_string(), dir.to_string()), resolved.to_string());
    }

    pub fn listing(&self, host: &str, dir: &str) -> Option<&Listing> {
        self.listings
            .get(&(host.to_string(), dir.to_string()))
            .map(|listed| &listed.listing)
    }

    pub fn request_bookmarks(&mut self, host: &str) {
        if self.bookmarks_inflight.contains(host) {
            return;
        }
        if self.bookmarks.get(host).is_some_and(|held| !held.stale()) {
            return;
        }
        self.bookmarks_inflight.insert(host.to_string());

        let host_key = host.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let rows = read_bookmarks(&host_key);
            let _ = tx.send(Done::Bookmarks {
                host: host_key,
                rows,
            });
        });
    }

    pub fn invalidate_bookmarks(&mut self) {
        self.bookmarks.clear();
    }

    pub fn request_listing(&mut self, host: &str, dir: &str) {
        let key = (host.to_string(), dir.to_string());
        if self.listings.get(&key).is_some_and(|held| !held.stale()) {
            return;
        }
        self.remember_listing(key.clone(), Listing::Pending);

        let dir = dir.to_string();
        let tx = self.tx.clone();
        if let Some((link, place)) = self.remote_place(host) {
            std::thread::spawn(move || {
                let (listing, dir) = remote_listing(&link, &place, &dir);
                let _ = tx.send(Done::Listing { key, listing, dir });
            });
            return;
        }
        std::thread::spawn(move || {
            let listing = match crate::git::list_dir_entries(&dir) {
                crate::git::DirListing::Missing => {
                    Listing::Failed(format!("No such directory: {dir}"))
                }
                crate::git::DirListing::Entries(entries) => Listing::Ready(
                    entries
                        .into_iter()
                        .map(|(name, is_git)| BrowseEntry { name, is_git })
                        .collect(),
                ),
            };
            let _ = tx.send(Done::Listing {
                key,
                listing,
                dir: None,
            });
        });
    }

    fn remember_listing(&mut self, key: PathKey, listing: Listing) {
        self.listings.insert(
            key,
            Listed {
                at: std::time::Instant::now(),
                listing,
            },
        );
    }

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
                        },
                    );
                }
                Done::Listing { key, listing, dir } => {
                    if let Some(dir) = dir {
                        self.resolved.insert(key.clone(), dir);
                    }
                    self.remember_listing(key, listing);
                }
                Done::Branches {
                    repo,
                    listing,
                    fetched,
                } => {
                    self.branches_inflight.remove(&repo);
                    match fetched {
                        Some(Some(why)) => {
                            self.fetch_notes.insert(repo.clone(), why);
                        }
                        Some(None) => {
                            self.fetch_notes.remove(&repo);
                        }
                        None => {}
                    }
                    self.branches.insert(
                        repo,
                        BranchesHeld {
                            at: std::time::Instant::now(),
                            listing,
                        },
                    );
                }
            }
            changed = true;
        }
        changed
    }

    pub fn serve(&mut self, wants: &Wants) {
        let wants = wants.clone();
        if let Some(host) = wants.bookmarks {
            self.request_bookmarks(&host);
        }
        if let Some((host, dir)) = wants.browse {
            self.request_listing(&host, &dir);
        }
        self.fetched
            .retain(|repo| wants.branches.as_ref() == Some(repo));
        if let Some(repo) = wants.branches {
            self.request_branches(&repo);
        }
    }

    #[doc(hidden)]
    pub fn set_branches_for_test(&mut self, repo: &str, listing: BranchListing) {
        self.branches.insert(
            repo.to_string(),
            BranchesHeld {
                at: std::time::Instant::now(),
                listing,
            },
        );
    }

    #[doc(hidden)]
    pub fn set_fetch_note_for_test(&mut self, repo: &str, why: &str) {
        self.fetch_notes.insert(repo.to_string(), why.to_string());
    }

    #[doc(hidden)]
    pub fn set_bookmarks_for_test(&mut self, host: &str, rows: Vec<BookmarkRow>) {
        self.bookmarks.insert(
            host.to_string(),
            Remembered {
                at: std::time::Instant::now(),
                rows,
            },
        );
    }

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

fn ask_place(
    link: &crate::fleet_link::FleetLink,
    args: &[&str],
    verb: &str,
) -> Result<serde_json::Value, String> {
    let out = link.bounded_output(args, link.probe_timeout())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr
            .lines()
            .find_map(|line| line.strip_prefix("error: "))
            .unwrap_or(stderr.trim())
            .to_string();
        if why.contains(&format!("does not offer {verb}")) {
            return Err(format!(
                "{} cannot {verb} here (its runtime does not offer it): type a path",
                link.name()
            ));
        }
        return Err(why);
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("fleet runtime {verb} answered no JSON: {e}"))
}

fn remote_listing(
    link: &crate::fleet_link::FleetLink,
    place: &str,
    dir: &str,
) -> (Listing, Option<String>) {
    let home = dir == "~";
    let args: &[&str] = if home {
        &["runtime", "browse", place, "--json"]
    } else {
        &["runtime", "browse", place, dir, "--json"]
    };
    match ask_place(link, args, "browse") {
        Ok(answer) => {
            let resolved = home
                .then(|| answer["dir"].as_str().map(str::to_string))
                .flatten();
            (listing_of(&answer), resolved)
        }
        Err(why) => (Listing::Failed(why), None),
    }
}

fn listing_of(answer: &serde_json::Value) -> Listing {
    Listing::Ready(
        answer["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some(BrowseEntry {
                    name: entry["name"].as_str()?.to_string(),
                    is_git: entry["repo"].as_bool().unwrap_or(false),
                })
            })
            .collect(),
    )
}

fn remote_branches(link: &crate::fleet_link::FleetLink, place: &str, repo: &str) -> BranchListing {
    match ask_place(
        link,
        &["runtime", "branches", place, repo, "--json"],
        "branches",
    ) {
        Ok(answer) => BranchListing::Ready(crate::git::Branches {
            default: answer["default"].as_str().map(str::to_string),
            names: answer["names"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect(),
            worktrees: Vec::new(),
        }),
        Err(why) => BranchListing::Failed(why),
    }
}

fn read_bookmarks(host: &str) -> Vec<BookmarkRow> {
    let bookmarks = super::bookmarks::list(host);
    if !host.is_empty() {
        return flatten(&bookmarks, &HashMap::new());
    }
    let mut rows = flatten(&bookmarks, &scan_parents(&bookmarks));
    offer_interface_dir(&mut rows);
    rows
}

fn offer_interface_dir(rows: &mut Vec<BookmarkRow>) {
    let Ok((dir, _chosen, _report)) = super::bundled::resolve(false) else {
        return;
    };
    if !dir.is_dir() || rows.iter().any(|row| row.path == dir.to_string_lossy()) {
        return;
    }
    let is_git = Some(dir.join(".git").exists());
    let mut offered = row(&dir, None, false, is_git);
    offered.label = Some("Thurbox interface — edit your panes".to_string());
    offered.offered = true;
    rows.insert(0, offered);
}

fn flatten(bookmarks: &[Bookmark], scanned: &HashMap<PathBuf, Vec<PathBuf>>) -> Vec<BookmarkRow> {
    let persisted = group_persisted(bookmarks);

    let covered: std::collections::HashSet<&PathBuf> = scanned
        .values()
        .flatten()
        .chain(persisted.values().flatten().map(|child| &child.repo_path))
        .collect();

    let mut rows = Vec::new();
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

fn scan_parents(bookmarks: &[Bookmark]) -> HashMap<PathBuf, Vec<PathBuf>> {
    bookmarks
        .iter()
        .filter(|bookmark| bookmark.is_parent)
        .filter(|bookmark| std::fs::read_dir(&bookmark.repo_path).is_ok())
        .map(|bookmark| {
            (
                bookmark.repo_path.clone(),
                crate::git::scan_child_repos(&bookmark.repo_path),
            )
        })
        .collect()
}

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
    #[test]
    fn only_this_machines_local_place_is_read_here() {
        let store = super::RepoStore::with_fleets(&[crate::fleet_link::FleetLink::local("fleet")]);
        assert!(store.remote_place("local/local").is_none());
        assert!(store.remote_place("").is_none());
        assert!(store.remote_place("/srv").is_none(), "no Fleet named ``");
        let (link, place) = store
            .remote_place("local/mux/box")
            .expect("a place elsewhere");
        assert!(link.is_local());
        assert_eq!(place, "mux/box");
    }

    #[cfg(unix)]
    #[test]
    fn a_remote_fleets_place_is_read_through_its_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "case \"$3\" in\n\
             local) ;;\n\
             *) echo 'error: runtime mux does not offer browse' >&2; exit 1 ;;\n\
             esac\n\
             case \"$2\" in\n\
             browse) echo '{\"dir\":\"/srv\",\"entries\":[{\"name\":\"app\",\"repo\":true}]}' ;;\n\
             branches) echo '{\"names\":[\"feat\",\"main\"],\"default\":\"main\"}' ;;\n\
             esac",
        );
        crate::paths::with_path(remote.path(), || {
            let mut store = super::RepoStore::with_fleets(std::slice::from_ref(&remote.link));
            store.request_listing("devbox/local", "/srv");
            store.request_listing("devbox/local", "~");
            store.request_listing("devbox/mux/vm", "/srv");
            store.request_branches("devbox/local\0/srv/app");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                store.poll();
                let done = [
                    ("devbox/local", "/srv"),
                    ("devbox/local", "~"),
                    ("devbox/mux/vm", "/srv"),
                ]
                .iter()
                .all(|(h, d)| !matches!(store.listing(h, d), Some(super::Listing::Pending)))
                    && !matches!(
                        store.branches("devbox/local\0/srv/app"),
                        Some(super::BranchListing::Pending)
                    );
                if done {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "{:?}", remote.calls());
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(
                store.listing("devbox/local", "/srv"),
                Some(&super::Listing::Ready(vec![super::BrowseEntry {
                    name: "app".into(),
                    is_git: true,
                }]))
            );
            assert!(
                matches!(store.listing("devbox/mux/vm", "/srv"),
                    Some(super::Listing::Failed(why)) if why.contains("devbox cannot browse here") && why.contains("type a path")),
                "{:?}",
                store.listing("devbox/mux/vm", "/srv")
            );
            assert_eq!(store.resolved_dir("devbox/local", "~"), Some("/srv"));
            match store.branches("devbox/local\0/srv/app") {
                Some(super::BranchListing::Ready(found)) => {
                    assert_eq!(found.names, ["feat", "main"]);
                    assert_eq!(found.default.as_deref(), Some("main"));
                }
                other => panic!("{other:?}"),
            }
        });
        assert!(
            remote.called("runtime browse local /srv --json"),
            "{:?}",
            remote.calls()
        );
        assert!(
            remote.called("runtime browse local --json"),
            "{:?}",
            remote.calls()
        );
        assert!(
            !remote
                .calls()
                .iter()
                .any(|c| c.contains("browse local '~'")),
            "{:?}",
            remote.calls()
        );
    }

    use super::*;

    #[test]
    fn nothing_is_known_before_it_is_asked_for() {
        let store = RepoStore::new();
        assert!(store.bookmarks("").is_none());
        assert!(store.listing("", "/tmp").is_none());
    }

    #[test]
    fn requesting_a_listing_returns_at_once_and_reads_pending() {
        let mut store = RepoStore::new();
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
        assert_ne!(Listing::Pending, Listing::Ready(Vec::new()));
    }

    #[test]
    fn a_second_request_does_not_relist() {
        let mut store = RepoStore::new();
        store.request_listing("", "/tmp");
        store.set_listing_for_test("", "/tmp", Listing::Ready(Vec::new()));
        store.request_listing("", "/tmp");
        assert_eq!(store.listing("", "/tmp"), Some(&Listing::Ready(Vec::new())));
    }

    fn held(listing: Listing, age: std::time::Duration) -> Listed {
        Listed {
            at: std::time::Instant::now() - age,
            listing,
        }
    }

    #[test]
    fn a_settled_listing_is_read_again_once_it_is_old() {
        assert!(!held(Listing::Ready(Vec::new()), std::time::Duration::ZERO).stale());
        assert!(held(Listing::Ready(Vec::new()), LISTING_TTL).stale());
        assert!(held(Listing::Failed("gone".into()), LISTING_TTL).stale());
    }

    #[test]
    fn a_pending_listing_is_never_asked_for_twice() {
        assert!(!held(Listing::Pending, LISTING_TTL * 10).stale());
    }

    #[test]
    fn an_old_listing_is_replaced_by_a_fresh_request() {
        let mut store = RepoStore::new();
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

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = crate::git::git_program()
            .current_dir(dir)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn advance(upstream: &std::path::Path, file: &str) -> String {
        std::fs::write(upstream.join(file), file).expect("write");
        git(upstream, &["add", file]);
        git(upstream, &["commit", "-q", "-m", file]);
        git(upstream, &["rev-parse", "HEAD"])
    }

    fn settle(store: &mut RepoStore, repo: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            store.poll();
            if !store.branches_inflight.contains(repo) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the branches of {repo} never arrived");
    }

    fn age_branches(store: &mut RepoStore, repo: &str) {
        let held = store.branches.get_mut(repo).expect("held");
        held.at = std::time::Instant::now() - LISTING_TTL;
    }

    #[test]
    fn branches_fetch_once_per_opening_and_rereads_only_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let upstream = dir.path().join("upstream");
        std::fs::create_dir_all(&upstream).expect("mkdir");
        git(&upstream, &["init", "-q"]);
        advance(&upstream, "one");
        git(dir.path(), &["clone", "-q", "upstream", "clone"]);
        let clone = dir.path().join("clone");
        let repo = clone.display().to_string();
        let origin_main = || git(&clone, &["rev-parse", "origin/main"]);
        let open = Wants::default().with_branches(Some(repo.clone()));
        let closed = Wants::default();
        let mut store = RepoStore::new();

        let first = advance(&upstream, "two");
        store.serve(&open);
        assert_eq!(store.branches(&repo), Some(&BranchListing::Pending));
        settle(&mut store, &repo);
        assert_eq!(origin_main(), first, "the opening fetches");

        let second = advance(&upstream, "three");
        age_branches(&mut store, &repo);
        store.serve(&open);
        assert!(
            matches!(store.branches(&repo), Some(BranchListing::Ready(_))),
            "a re-read keeps the list on screen"
        );
        settle(&mut store, &repo);
        assert_eq!(origin_main(), first, "a re-read does not fetch");

        store.serve(&closed);
        store.serve(&open);
        assert_eq!(store.branches(&repo), Some(&BranchListing::Pending));
        settle(&mut store, &repo);
        assert_eq!(origin_main(), second, "opening again fetches again");
    }

    #[test]
    fn a_missing_directory_reports_rather_than_hanging() {
        let mut store = RepoStore::new();
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
        let mut store = RepoStore::new();
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
        let mut store = RepoStore::new();
        store.request_bookmarks("");
        assert!(store.bookmarks_inflight.contains(""));
        store.request_bookmarks("");
        assert_eq!(store.bookmarks_inflight.len(), 1);
    }

    #[test]
    fn invalidating_bookmarks_drops_what_was_read() {
        let mut store = RepoStore::new();
        store.set_bookmarks_for_test(
            "",
            vec![row(Path::new("/src/thing"), None, false, Some(true))],
        );
        store.invalidate_bookmarks();
        assert!(store.bookmarks("").is_none());
    }

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

    fn no_scan() -> HashMap<PathBuf, Vec<PathBuf>> {
        HashMap::new()
    }

    fn scanned(folder: &str, children: &[&str]) -> HashMap<PathBuf, Vec<PathBuf>> {
        HashMap::from([(
            PathBuf::from(folder),
            children.iter().map(PathBuf::from).collect(),
        )])
    }

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
        let rows = flatten(
            &[
                saved("/src", true, None),
                saved("/src/one", false, Some("/src")),
                saved("/src/two", false, Some("/src")),
            ],
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
    fn a_repository_a_folder_covers_is_not_listed_twice() {
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
        assert_eq!(wants.bookmarks.as_deref(), Some(""));
        assert_eq!(wants.browse, Some(("ssh:box".into(), "/srv/repos".into())));
    }

    #[test]
    fn a_malformed_or_empty_want_asks_for_nothing() {
        assert_eq!(Wants::new(None, Some("/srv/repos".into())).browse, None);
        let wants = Wants::new(None, Some("ssh:box\0".into()));
        assert_eq!(wants.bookmarks, None);
        assert_eq!(wants.browse, None);
    }

    #[test]
    fn the_interface_directory_is_offered_first() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let ui = home.path().join("ui");
        std::fs::create_dir_all(ui.join("mods")).expect("mkdir");
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
        let label = rows[0].label.as_deref().expect("the offered row is named");
        assert!(
            label.contains("interface"),
            "the name says what it is: {label}"
        );
        assert!(rows[0].offered, "an offered row says so");
        std::env::remove_var("THURBOX_UI_DIR");
    }

    #[test]
    fn another_hosts_list_offers_no_local_interface_directory() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let ui = home.path().join("ui");
        std::fs::create_dir_all(ui.join("mods")).expect("mkdir");
        std::env::set_var("THURBOX_UI_DIR", &ui);
        assert!(read_bookmarks("").iter().any(|row| row.offered));
        assert!(
            !read_bookmarks("devbox").iter().any(|row| row.offered),
            "another host's list offers this machine's interface"
        );
        std::env::remove_var("THURBOX_UI_DIR");
    }

    #[test]
    fn a_bookmark_of_your_own_keeps_its_place() {
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
