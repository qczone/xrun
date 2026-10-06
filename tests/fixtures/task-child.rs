use std::{
    io::{Read, Write},
    time::Duration,
};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a[0].as_str() {
        "echo" => {
            for s in &a[1..] {
                println!("<{s}>");
            }
        }
        "input" => {
            let mut b = vec![];
            std::io::stdin().read_to_end(&mut b).unwrap();
            std::io::stdout().write_all(&b).unwrap();
        }
        "env" => print!("{}", std::env::var("XRUN_TEST_SECRET").unwrap()),
        "env-values" => {
            for name in &a[1..] {
                println!("{name}={}", std::env::var(name).unwrap_or_default());
            }
        }
        "argv0" => print!(
            "{}",
            std::path::Path::new(&std::env::args().next().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
        ),
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "log-gate" => {
            let mut gate = std::net::TcpStream::connect(&a[1]).unwrap();
            let mut command = [0];
            while gate.read_exact(&mut command).is_ok() {
                println!("gated output");
                std::io::stdout().flush().unwrap();
            }
        }
        "finish-later" => {
            std::thread::sleep(Duration::from_secs(1));
            print!("completed");
        }
        "exit" => {
            eprintln!("error bytes");
            std::process::exit(7);
        }
        "detached" => {
            let _ = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("sleep")
                .spawn()
                .unwrap();
            print!("parent done");
        }
        _ => panic!(),
    }
}
