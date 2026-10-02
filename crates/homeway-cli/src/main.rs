//! homeway-cli —— 测试/运维命令面（占位）。
//!
//! R1 实装 `homeway-cli connect --token <hmw1…>`（垂直切片）；当前提供 token 解析
//! 冒烟（消费 homeway-core，Rust↔Go 向量互证的生产面入口形态）。

use homeway_core::token;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("token") => match args.get(2) {
            Some(s) => match token::decode(s) {
                Ok(t) => {
                    println!("peer_id  = {}", hex_str(t.peer_id.as_bytes()));
                    println!("secret   = {}", hex_str(t.secret.as_bytes()));
                    for e in &t.endpoints {
                        println!("endpoint = {} ({:?})", e.addr, e.kind);
                    }
                }
                Err(e) => {
                    eprintln!("解析失败：{e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("用法：homeway-cli token <hmw1…>");
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("homeway-cli（占位）——可用：token <hmw1…>；R1 实装 connect");
        }
    }
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
