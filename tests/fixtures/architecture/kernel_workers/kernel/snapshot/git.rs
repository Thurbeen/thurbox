pub fn request() {
    std::thread::spawn(|| crate::git::stats());
}
