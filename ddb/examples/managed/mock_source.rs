#[derive(Debug)]
struct Request {
    id: u64,
}

fn handle_request(request: Request) -> u64 {
    let base = 40;
    let increment = 2;
    base + increment + request.id
}

fn main() {
    let result = handle_request(Request { id: 0 });
    println!("result={result}");
}
