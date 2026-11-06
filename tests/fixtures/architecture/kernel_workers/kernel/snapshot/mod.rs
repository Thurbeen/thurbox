mod git;

// A comment naming std::thread::spawn is not a spawn.
pub fn refresh() {
    let _ = "crate::git::stats";
    std::thread::spawn(|| {});
}

#[cfg(test)]
mod tests {
    #[test]
    fn waits() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
