fn seed(path: &std::path::Path) {
    std::fs::File::create(path).unwrap();
}
