use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::inject::Failure;
use super::probe::Refusal;

const REPLY_WITHIN: Duration = Duration::from_secs(1);
const NORMAL_WITHIN: Duration = Duration::from_millis(200);
const MODE_POLL: Duration = Duration::from_millis(10);
const DEEPEST: usize = 16;
const TO_NORMAL: &str = "<C-\\><C-N>";
const JUMP: &str = "local file, line = ...
vim.cmd(\"drop \" .. vim.fn.fnameescape(file))
vim.api.nvim_win_set_cursor(0, { math.min(line, vim.api.nvim_buf_line_count(0)), 0 })
vim.cmd(\"normal! zv\")";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Integer(i64),
    Text(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg<'a> {
    Int(u64),
    Text(&'a str),
    List(Vec<Arg<'a>>),
}

pub fn encode(arg: &Arg<'_>, out: &mut Vec<u8>) {
    match arg {
        Arg::Int(value) => {
            out.push(0xcf);
            out.extend_from_slice(&value.to_be_bytes());
        }
        Arg::Text(text) => {
            out.push(0xdb);
            out.extend_from_slice(&length(text.len()).to_be_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        Arg::List(items) => {
            out.push(0xdd);
            out.extend_from_slice(&length(items.len()).to_be_bytes());
            for item in items {
                encode(item, out);
            }
        }
    }
}

fn length(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn bytes<const N: usize>(input: &mut impl Read) -> Result<[u8; N]> {
    let mut buffer = [0; N];
    input.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn size(input: &mut impl Read, width: u8) -> Result<u64> {
    Ok(match width {
        1 => u64::from(u8::from_be_bytes(bytes(input)?)),
        2 => u64::from(u16::from_be_bytes(bytes(input)?)),
        4 => u64::from(u32::from_be_bytes(bytes(input)?)),
        _ => u64::from_be_bytes(bytes(input)?),
    })
}

fn signed(input: &mut impl Read, width: u8) -> Result<i64> {
    Ok(match width {
        1 => i64::from(i8::from_be_bytes(bytes(input)?)),
        2 => i64::from(i16::from_be_bytes(bytes(input)?)),
        4 => i64::from(i32::from_be_bytes(bytes(input)?)),
        _ => i64::from_be_bytes(bytes(input)?),
    })
}

fn skip(input: &mut impl Read, count: u64) -> Result<Value> {
    let skipped = io::copy(&mut input.take(count), &mut io::sink())?;
    if skipped < count {
        bail!("nvim's message ended early");
    }
    Ok(Value::Other)
}

fn text(input: &mut impl Read, count: u64) -> Result<Value> {
    let mut raw = Vec::new();
    input.take(count).read_to_end(&mut raw)?;
    if u64::try_from(raw.len()) != Ok(count) {
        bail!("nvim's message ended early");
    }
    Ok(Value::Text(String::from_utf8_lossy(&raw).into_owned()))
}

fn array(input: &mut impl Read, count: u64, depth: usize) -> Result<Value> {
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(decode(input, depth + 1)?);
    }
    Ok(Value::Array(items))
}

fn map(input: &mut impl Read, count: u64, depth: usize) -> Result<Value> {
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push((decode(input, depth + 1)?, decode(input, depth + 1)?));
    }
    Ok(Value::Map(entries))
}

pub fn decode(input: &mut impl Read, depth: usize) -> Result<Value> {
    if depth > DEEPEST {
        bail!("nvim's message is nested too deeply");
    }
    let [marker] = bytes(input)?;
    Ok(match marker {
        0x00..=0x7f => Value::Integer(i64::from(marker)),
        0x80..=0x8f => map(input, u64::from(marker & 0x0f), depth)?,
        0x90..=0x9f => array(input, u64::from(marker & 0x0f), depth)?,
        0xa0..=0xbf => text(input, u64::from(marker & 0x1f))?,
        0xc0 => Value::Nil,
        0xc2 => Value::Bool(false),
        0xc3 => Value::Bool(true),
        0xc4 => {
            let count = size(input, 1)?;
            skip(input, count)?
        }
        0xc5 => {
            let count = size(input, 2)?;
            skip(input, count)?
        }
        0xc6 => {
            let count = size(input, 4)?;
            skip(input, count)?
        }
        0xc7 => {
            let count = size(input, 1)?;
            skip(input, count + 1)?
        }
        0xc8 => {
            let count = size(input, 2)?;
            skip(input, count + 1)?
        }
        0xc9 => {
            let count = size(input, 4)?;
            skip(input, count + 1)?
        }
        0xca => skip(input, 4)?,
        0xcb => skip(input, 8)?,
        0xcc => Value::Integer(i64::try_from(size(input, 1)?)?),
        0xcd => Value::Integer(i64::try_from(size(input, 2)?)?),
        0xce => Value::Integer(i64::try_from(size(input, 4)?)?),
        0xcf => i64::try_from(size(input, 8)?).map_or(Value::Other, Value::Integer),
        0xd0 => Value::Integer(signed(input, 1)?),
        0xd1 => Value::Integer(signed(input, 2)?),
        0xd2 => Value::Integer(signed(input, 4)?),
        0xd3 => Value::Integer(signed(input, 8)?),
        0xd4 => skip(input, 2)?,
        0xd5 => skip(input, 3)?,
        0xd6 => skip(input, 5)?,
        0xd7 => skip(input, 9)?,
        0xd8 => skip(input, 17)?,
        0xd9 => {
            let count = size(input, 1)?;
            text(input, count)?
        }
        0xda => {
            let count = size(input, 2)?;
            text(input, count)?
        }
        0xdb => {
            let count = size(input, 4)?;
            text(input, count)?
        }
        0xdc => {
            let count = size(input, 2)?;
            array(input, count, depth)?
        }
        0xdd => {
            let count = size(input, 4)?;
            array(input, count, depth)?
        }
        0xde => {
            let count = size(input, 2)?;
            map(input, count, depth)?
        }
        0xdf => {
            let count = size(input, 4)?;
            map(input, count, depth)?
        }
        0xe0..=0xff => Value::Integer(i64::from(i8::from_be_bytes([marker]))),
        0xc1 => bail!("nvim sent the unused msgpack marker 0xc1"),
    })
}

fn describe(error: &Value) -> String {
    match error {
        Value::Array(parts) => parts
            .iter()
            .find_map(|part| match part {
                Value::Text(message) => Some(message.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "an error".to_owned()),
        Value::Text(message) => message.clone(),
        _ => "an error".to_owned(),
    }
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("could not reach {}", socket.display()))?;
        stream.set_read_timeout(Some(REPLY_WITHIN))?;
        stream.set_write_timeout(Some(REPLY_WITHIN))?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next: 0,
        })
    }

    pub fn call(&mut self, method: &str, params: Vec<Arg<'_>>) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut request = Vec::new();
        encode(
            &Arg::List(vec![
                Arg::Int(0),
                Arg::Int(id),
                Arg::Text(method),
                Arg::List(params),
            ]),
            &mut request,
        );
        self.writer.write_all(&request)?;
        loop {
            let message = decode(&mut self.reader, 0)
                .with_context(|| format!("nvim did not answer {method}"))?;
            if let Value::Array(parts) = message
                && let [Value::Integer(1), Value::Integer(answered), error, result] =
                    parts.as_slice()
                && u64::try_from(*answered) == Ok(id)
            {
                return match error {
                    Value::Nil => Ok(result.clone()),
                    error => Err(anyhow!("nvim: {}", describe(error))),
                };
            }
        }
    }

    fn mode(&mut self) -> Result<(String, bool)> {
        let Value::Map(entries) = self.call("nvim_get_mode", Vec::new())? else {
            bail!("nvim_get_mode gave no map");
        };
        let field = |name: &str| {
            entries
                .iter()
                .find(|(key, _)| *key == Value::Text(name.to_owned()))
                .map(|(_, value)| value)
        };
        let Some(Value::Text(mode)) = field("mode") else {
            bail!("nvim_get_mode gave no mode");
        };
        Ok((mode.clone(), field("blocking") == Some(&Value::Bool(true))))
    }
}

#[derive(Debug)]
pub enum RpcError {
    Unreachable(anyhow::Error),
    Failed(Failure),
}

pub trait Rpc {
    fn jump(&self, socket: &Path, file: &Path, line: u32) -> Result<(), RpcError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Socket;

impl Rpc for Socket {
    fn jump(&self, socket: &Path, file: &Path, line: u32) -> Result<(), RpcError> {
        let mut nvim = Client::connect(socket).map_err(RpcError::Unreachable)?;
        steer(&mut nvim, file, line)
    }
}

fn failed(error: anyhow::Error) -> RpcError {
    RpcError::Failed(Failure::Send(error))
}

fn normal(nvim: &mut Client) -> Result<(), RpcError> {
    let started = Instant::now();
    let mut asked = false;
    loop {
        let (mode, blocking) = nvim.mode().map_err(failed)?;
        if blocking {
            return Err(RpcError::Failed(Failure::Refused(Refusal::Blocked)));
        }
        if mode == "n" {
            return Ok(());
        }
        if started.elapsed() >= NORMAL_WITHIN {
            return Err(failed(anyhow!("nvim stayed in mode {mode}")));
        }
        if !asked {
            nvim.call("nvim_input", vec![Arg::Text(TO_NORMAL)])
                .map_err(failed)?;
            asked = true;
        }
        thread::sleep(MODE_POLL);
    }
}

fn steer(nvim: &mut Client, file: &Path, line: u32) -> Result<(), RpcError> {
    let path = file
        .to_str()
        .ok_or_else(|| failed(anyhow!("the path is not UTF-8")))?;
    normal(nvim)?;
    nvim.call(
        "nvim_exec_lua",
        vec![
            Arg::Text(JUMP),
            Arg::List(vec![Arg::Text(path), Arg::Int(u64::from(line))]),
        ],
    )
    .map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread::JoinHandle;
    use std::time::SystemTime;

    use super::*;

    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Nil | Value::Other => out.push(0xc0),
            Value::Bool(flag) => out.push(if *flag { 0xc3 } else { 0xc2 }),
            Value::Integer(number) => {
                out.push(0xd3);
                out.extend_from_slice(&number.to_be_bytes());
            }
            Value::Text(text) => encode(&Arg::Text(text), out),
            Value::Array(items) => {
                out.push(0x90 | u8::try_from(items.len()).unwrap());
                items.iter().for_each(|item| write(item, out));
            }
            Value::Map(entries) => {
                out.push(0x80 | u8::try_from(entries.len()).unwrap());
                for (key, value) in entries {
                    write(key, out);
                    write(value, out);
                }
            }
        }
    }

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    fn mode(mode: &str, blocking: bool) -> Value {
        Value::Map(vec![
            (text("mode"), text(mode)),
            (text("blocking"), Value::Bool(blocking)),
        ])
    }

    type Calls = Vec<(String, Value)>;
    type Answer = Option<Result<Value, Value>>;

    fn serve(
        mut answer: impl FnMut(&str) -> Answer + Send + 'static,
    ) -> (PathBuf, JoinHandle<Calls>) {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let socket = std::env::temp_dir().join(format!("termleaf-nvim-{nanos}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut calls = Vec::new();
            while let Ok(Value::Array(request)) = decode(&mut reader, 0)
                && let [_, id, Value::Text(method), params] = request.as_slice()
            {
                calls.push((method.clone(), params.clone()));
                let Some(outcome) = answer(method) else {
                    break;
                };
                let (error, result) = match outcome {
                    Ok(result) => (Value::Nil, result),
                    Err(error) => (error, Value::Nil),
                };
                let mut out = Vec::new();
                write(
                    &Value::Array(vec![Value::Integer(2), text("redraw")]),
                    &mut out,
                );
                write(
                    &Value::Array(vec![
                        Value::Integer(1),
                        Value::Integer(0),
                        Value::Nil,
                        Value::Nil,
                    ]),
                    &mut out,
                );
                write(
                    &Value::Array(vec![Value::Integer(1), id.clone(), error, result]),
                    &mut out,
                );
                writer.write_all(&out).unwrap();
            }
            calls
        });
        (socket, server)
    }

    fn jump(
        answer: impl FnMut(&str) -> Answer + Send + 'static,
        file: &Path,
    ) -> (Result<(), RpcError>, Vec<String>, Calls) {
        let (socket, server) = serve(answer);
        let outcome = Socket.jump(&socket, file, 77);
        let calls = server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        let methods = calls.iter().map(|(method, _)| method.clone()).collect();
        (outcome, methods, calls)
    }

    fn failure(outcome: Result<(), RpcError>) -> String {
        match outcome {
            Ok(()) => "sent".to_owned(),
            Err(RpcError::Failed(failure)) => failure.to_string(),
            Err(RpcError::Unreachable(error)) => format!("unreachable: {error}"),
        }
    }

    #[test]
    fn every_msgpack_marker_is_decoded_or_skipped() {
        let mut blob = vec![0x05, 0x81, 0xa1, b'k', 0xc0, 0x91, 0xc3, 0xa2, b'h', b'i'];
        blob.extend([0xc0, 0xc2, 0xc3]);
        blob.extend([0xc4, 1, 9, 0xc5, 0, 1, 9, 0xc6, 0, 0, 0, 1, 9]);
        blob.extend([0xc7, 1, 7, 9, 0xc8, 0, 1, 7, 9, 0xc9, 0, 0, 0, 1, 7, 9]);
        blob.extend([0xca, 0, 0, 0, 0, 0xcb, 0, 0, 0, 0, 0, 0, 0, 0]);
        blob.extend([0xcc, 0xff, 0xcd, 0xff, 0xff, 0xce, 0xff, 0xff, 0xff, 0xff]);
        blob.push(0xcf);
        blob.extend(i64::MAX.to_be_bytes());
        blob.push(0xcf);
        blob.extend(u64::MAX.to_be_bytes());
        blob.extend([
            0xd0, 0xff, 0xd1, 0xff, 0xfe, 0xd2, 0xff, 0xff, 0xff, 0xfd, 0xd3,
        ]);
        blob.extend((-4_i64).to_be_bytes());
        for (marker, width) in [(0xd4, 2), (0xd5, 3), (0xd6, 5), (0xd7, 9), (0xd8, 17)] {
            blob.push(marker);
            blob.extend(vec![0; width]);
        }
        blob.extend([0xd9, 1, b'a', 0xda, 0, 1, b'b', 0xdb, 0, 0, 0, 1, b'c']);
        blob.extend([0xdc, 0, 1, 1, 0xdd, 0, 0, 0, 1, 2]);
        blob.extend([0xde, 0, 1, 1, 2, 0xdf, 0, 0, 0, 1, 3, 4, 0xe0]);
        let mut input = Cursor::new(blob);
        let mut values = Vec::new();
        while input.position() < u64::try_from(input.get_ref().len()).unwrap() {
            values.push(decode(&mut input, 0).unwrap());
        }
        let int = Value::Integer;
        let mut expected = vec![
            int(5),
            Value::Map(vec![(text("k"), Value::Nil)]),
            Value::Array(vec![Value::Bool(true)]),
            text("hi"),
            Value::Nil,
            Value::Bool(false),
            Value::Bool(true),
        ];
        expected.extend(vec![Value::Other; 8]);
        expected.extend([int(255), int(65535), int(4_294_967_295), int(i64::MAX)]);
        expected.extend([Value::Other, int(-1), int(-2), int(-3), int(-4)]);
        expected.extend(vec![Value::Other; 5]);
        expected.extend([text("a"), text("b"), text("c")]);
        expected.extend([Value::Array(vec![int(1)]), Value::Array(vec![int(2)])]);
        expected.extend([
            Value::Map(vec![(int(1), int(2))]),
            Value::Map(vec![(int(3), int(4))]),
            int(-32),
        ]);
        assert_eq!(values, expected);
    }

    #[test]
    fn malformed_or_hostile_messages_are_errors() {
        let mut nested = vec![0x91; DEEPEST + 1];
        nested.push(0x00);
        for blob in [vec![0xc1], nested, vec![0xa3, b'a'], vec![0xc4, 5, 0]] {
            assert!(decode(&mut Cursor::new(blob), 0).is_err());
        }
    }

    #[test]
    fn errors_are_described_by_their_message() {
        let described = [
            Value::Array(vec![Value::Integer(0), text("E37: No write")]),
            Value::Array(vec![Value::Integer(0)]),
            text("bad"),
            Value::Nil,
        ]
        .map(|error| describe(&error));
        assert_eq!(described, ["E37: No write", "an error", "bad", "an error"]);
    }

    #[test]
    fn a_normal_mode_neovim_drops_the_file_at_the_line() {
        let (outcome, methods, calls) = jump(
            |method| {
                Some(Ok(match method {
                    "nvim_get_mode" => mode("n", false),
                    _ => Value::Nil,
                }))
            },
            Path::new("/tmp/thesis/ch5.tex"),
        );
        assert_eq!(failure(outcome), "sent");
        assert_eq!(methods, ["nvim_get_mode", "nvim_exec_lua"]);
        assert_eq!(
            calls[1].1,
            Value::Array(vec![
                text(JUMP),
                Value::Array(vec![text("/tmp/thesis/ch5.tex"), Value::Integer(77)]),
            ])
        );
    }

    #[test]
    fn insert_mode_is_left_for_normal_before_the_jump() {
        let mut modes = ["i", "n"].into_iter();
        let (outcome, methods, calls) = jump(
            move |method| {
                Some(Ok(match method {
                    "nvim_get_mode" => mode(modes.next().unwrap_or("n"), false),
                    _ => Value::Nil,
                }))
            },
            Path::new("/tmp/thesis/ch5.tex"),
        );
        assert_eq!(failure(outcome), "sent");
        assert_eq!(
            methods,
            [
                "nvim_get_mode",
                "nvim_input",
                "nvim_get_mode",
                "nvim_exec_lua"
            ]
        );
        assert_eq!(calls[1].1, Value::Array(vec![text(TO_NORMAL)]));
    }

    #[test]
    fn a_neovim_waiting_at_a_prompt_is_refused_before_any_input() {
        let (outcome, methods, _) = jump(
            |_| Some(Ok(mode("r", true))),
            Path::new("/tmp/thesis/ch5.tex"),
        );
        assert_eq!(failure(outcome), "refused: waiting at a prompt");
        assert_eq!(methods, ["nvim_get_mode"]);
    }

    #[test]
    fn a_neovim_that_stays_out_of_normal_mode_is_left_alone() {
        let (outcome, methods, _) = jump(
            |method| {
                Some(Ok(match method {
                    "nvim_get_mode" => mode("i", false),
                    _ => Value::Nil,
                }))
            },
            Path::new("/tmp/thesis/ch5.tex"),
        );
        assert_eq!(failure(outcome), "could not send: nvim stayed in mode i");
        assert!(!methods.contains(&"nvim_exec_lua".to_owned()));
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "nvim_input")
                .count(),
            1
        );
    }

    #[test]
    fn neovim_errors_and_odd_answers_fail_the_jump() {
        let answers: [fn(&str) -> Answer; 4] = [
            |method| match method {
                "nvim_get_mode" => Some(Ok(mode("n", false))),
                _ => Some(Err(text("E37: No write"))),
            },
            |_| Some(Ok(Value::Nil)),
            |_| Some(Ok(Value::Map(Vec::new()))),
            |_| None,
        ];
        let failures =
            answers.map(|answer| failure(jump(answer, Path::new("/tmp/thesis/ch5.tex")).0));
        assert_eq!(
            failures,
            [
                "could not send: nvim: E37: No write",
                "could not send: nvim_get_mode gave no map",
                "could not send: nvim_get_mode gave no mode",
                "could not send: nvim did not answer nvim_get_mode",
            ]
        );
    }

    #[test]
    fn a_path_that_is_not_utf8_is_never_sent() {
        let file = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.tex"));
        let (outcome, methods, _) = jump(|_| None, file);
        assert_eq!(failure(outcome), "could not send: the path is not UTF-8");
        assert!(methods.is_empty());
    }

    #[test]
    fn a_missing_socket_is_unreachable_so_typing_can_take_over() {
        let outcome = Socket.jump(
            Path::new("/nonexistent/termleaf.sock"),
            Path::new("/tmp/thesis/ch5.tex"),
            77,
        );
        assert!(failure(outcome).starts_with("unreachable: could not reach"));
    }
}
