use std::io::Write;

fn main() {
    let mut output = std::io::stdout().lock();
    let mut line = [b'x'; 50];
    line[49] = b'\n';
    let bursts = std::env::args().nth(1).as_deref() == Some("bursts");
    for index in 0..100_000 {
        output.write_all(&line).unwrap();
        if bursts && (index + 1) % 20 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    output.flush().unwrap();
}
