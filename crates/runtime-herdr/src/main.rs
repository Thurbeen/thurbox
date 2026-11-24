use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("view") {
        std::process::exit(runtime_herdr::view::run(&args[1..]));
    }
    let verb = args.first().cloned().unwrap_or_default();
    let mut request = Vec::new();
    let answer = match std::io::stdin().read_to_end(&mut request) {
        Ok(_) => runtime_herdr::answer(&verb, &request),
        Err(e) => runtime_herdr::unknown(format!("cannot read the request: {e}")),
    };
    println!("{answer}");
}
