mod git;
mod probe;

use crate::git as repo;

// A comment naming std::thread::spawn or crate::git::stats is not a reach.
pub fn refresh() {
    let _ = "crate::git::stats";
    git::request();
    std::thread::spawn(|| {});
    repo::stats();
}

#[cfg(test)]
mod tests {
    #[test]
    fn waits() {
        std::thread::sleep(std::time::Duration::from_millis(1));
        crate::git::stats();
    }
}
