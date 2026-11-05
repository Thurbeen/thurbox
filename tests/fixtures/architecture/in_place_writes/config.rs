pub fn save(path: &std::path::Path) {
    // std::fs::write(path, "in a comment");
    let _ = "fs::write(path, in a string)";
    let _ = myfs::write(path, "a different module");
    std::fs::write(path, "the production write").unwrap();
}

pub fn save_again(path: &std::path::Path) {
    std::fs::write(path, "a second production write").unwrap();
}

#[cfg(test)]
mod tests {
    fn seed(path: &std::path::Path) {
        std::fs::write(path, "a test write").unwrap();
    }
}
