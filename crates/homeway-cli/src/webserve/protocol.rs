use std::io::{Read, Write};

use super::Error;

pub(super) const MAX_FRAME: usize = 65536;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub(super) enum Op {
    Open = 1,
    Input = 2,
    Resize = 3,
    Output = 4,
    Close = 5,
    Error = 6,
    Exit = 7,
}

pub(super) fn read(reader: &mut impl Read) -> Result<(Op, Vec<u8>), Error> {
    let mut head = [0; 5];
    reader.read_exact(&mut head)?;
    let op = match head[0] {
        1 => Op::Open,
        2 => Op::Input,
        3 => Op::Resize,
        4 => Op::Output,
        5 => Op::Close,
        6 => Op::Error,
        7 => Op::Exit,
        _ => return Err(Error::Invalid("未知管理终端帧")),
    };
    let len = u32::from_be_bytes(head[1..].try_into().expect("定长头")) as usize;
    if len > MAX_FRAME {
        return Err(Error::Invalid("管理终端帧超过64KiB"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    validate(op, &payload)?;
    Ok((op, payload))
}

pub(super) fn write(writer: &mut impl Write, op: Op, body: &[u8]) -> Result<(), Error> {
    validate(op, body)?;
    if body.len() > MAX_FRAME {
        return Err(Error::Invalid("管理终端帧超过64KiB"));
    }
    let mut head = [0; 5];
    head[0] = op as u8;
    head[1..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    writer.write_all(&head)?;
    writer.write_all(body)?;
    Ok(())
}

pub(super) fn size(cols: u16, rows: u16) -> Result<[u8; 4], Error> {
    if !(1..=1000).contains(&cols) || !(1..=500).contains(&rows) {
        return Err(Error::Invalid("终端尺寸须为1..1000列、1..500行"));
    }
    let mut out = [0; 4];
    out[..2].copy_from_slice(&cols.to_be_bytes());
    out[2..].copy_from_slice(&rows.to_be_bytes());
    Ok(out)
}

pub(super) fn parse_size(data: &[u8]) -> Result<(u16, u16), Error> {
    if data.len() != 4 {
        return Err(Error::Invalid("终端尺寸帧长度错误"));
    }
    let cols = u16::from_be_bytes([data[0], data[1]]);
    let rows = u16::from_be_bytes([data[2], data[3]]);
    size(cols, rows)?;
    Ok((cols, rows))
}

fn validate(op: Op, body: &[u8]) -> Result<(), Error> {
    let valid = match op {
        Op::Open => body.len() == 5 && body[0] == 1 && parse_size(&body[1..]).is_ok(),
        Op::Input | Op::Output => !body.is_empty() && body.len() <= MAX_FRAME,
        Op::Resize => parse_size(body).is_ok(),
        Op::Close => body.is_empty(),
        Op::Error => {
            !body.is_empty()
                && body.len() <= 513
                && (1..=6).contains(&body[0])
                && std::str::from_utf8(&body[1..]).is_ok()
        }
        Op::Exit => body.len() == 4,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid("管理终端帧长度或载荷无效"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bytes(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn independent_terminal_vectors() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/webserve/terminal_frames.json"
        ))
        .unwrap();
        for case in oracle["valid"].as_array().unwrap() {
            let wire = bytes(case["wire_hex"].as_str().unwrap());
            let (op, payload) = read(&mut &wire[..]).unwrap();
            let mut encoded = Vec::new();
            write(&mut encoded, op, &payload).unwrap();
            assert_eq!(encoded, wire, "{}", case["name"]);
        }
        for case in oracle["invalid"].as_array().unwrap() {
            let wire = bytes(case["wire_hex"].as_str().unwrap());
            assert!(read(&mut &wire[..]).is_err(), "{}", case["name"]);
        }
    }
}
