mod git;

use crate::git as repo;
use crate::storage::Database as Db;

// A comment naming std::thread::spawn or crate::git::stats is not a reach.
pub fn hand_over(_db: Db) {}

pub fn refresh() {
    let _ = "crate::git::stats";
    let _ = "Database::open";
    git::request();
    std::thread::spawn(|| {});
    repo::stats();
    Db::open();
}

#[cfg(test)]
mod tests {
    #[test]
    fn waits() {
        std::thread::sleep(std::time::Duration::from_millis(1));
        crate::git::stats();
    }
}
