pub struct Thing;

#[cfg(all(test, unix))]
mod inline_tests {
    fn reach_back() {
        crate::a::production();
    }
}

#[cfg(test)]
mod tests;
