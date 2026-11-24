use std::io::Read;

fn main() {
    let verb = std::env::args().nth(1).unwrap_or_default();
    let mut request = Vec::new();
    let answer = match std::io::stdin().read_to_end(&mut request) {
        Ok(_) => runtime_herdr::answer(&verb, &request),
        Err(e) => runtime_herdr::unknown(format!("cannot read the request: {e}")),
    };
    println!("{answer}");
}
