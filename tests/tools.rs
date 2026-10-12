use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use ieum::{FileSystem, Limits, Options, format_source, parse_syntax};
use serde_json::{Value as Json, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ieum-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn put(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ieum(args: &[&str], cwd: &Path, stdin: &str) -> (Option<i32>, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ieum"))
        .args(args)
        .current_dir(cwd)
        .env("IEUM_TEST", "값")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (
        output.status.code(),
        text(output.stdout),
        text(output.stderr),
    )
}

fn execute(source: &str, options: &Options) -> String {
    let mut output = Vec::new();
    let result = ieum::execute(source.as_bytes(), &mut output, options);
    let mut text = String::from_utf8(output).unwrap();
    if let Err(error) = result {
        text.push_str(error.diagnostic.code);
    }
    text
}

#[test]
fn command_line() {
    let dir = TempDir::new("cli");
    dir.put(
        "도구.이음",
        "`수`를 두배하는 법:\n    (`수` * 2)를 반환한다.\n",
    );
    dir.put("자료.txt", "기준");
    dir.put(
        "시작.이음",
        "\"도구\"에서 두배하다를 가져온다.\n(3을 두배한 값)을 출력한다.\n(\"자료.txt\"를 읽은 값)을 출력한다.\n환경변수[\"IEUM_TEST\"]를 출력한다.\n",
    );
    dir.put("고장.이음", "1을 출력한다\n");
    dir.put("가져옴.이음", "\"고장\"에서 무엇을 가져온다.\n");
    dir.put("정리.이음", "1을   출력한다 .");
    let run = |args: &[&str], stdin: &str| ieum(args, &dir.0, stdin);

    assert_eq!(
        run(&["시작.이음"], ""),
        (Some(0), "6\n기준\n값\n".into(), String::new())
    );
    assert_eq!(
        run(&["run", "-"], "1을 출력한다.\n(1 // 0)을 출력한다.\n").0,
        Some(1)
    );
    let (code, stdout, stderr) = run(&["check", "가져옴.이음"], "");
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(stderr.contains("고장.이음:1:8: E-LAYOUT-006"), "{stderr}");
    assert_eq!(run(&["run", "-"], "실행을 3으로 끝낸다.\n").0, Some(3));
    assert_eq!(run(&["fmt", "-"], "1을   출력한다 .").1, "1을 출력한다.\n");
    assert_eq!(run(&["fmt", "--check", "정리.이음"], "").0, Some(1));
    assert_eq!(
        fs::read_to_string(dir.0.join("정리.이음")).unwrap(),
        "1을   출력한다 ."
    );
    assert!(run(&["dis", "도구.이음"], "").1.starts_with("<모듈>:"));
    assert_eq!(run(&["없음.이음"], "").0, Some(2));
    assert_eq!(run(&["--bogus"], "").0, Some(2));
    let (code, stdout, stderr) = run(
        &["repl"],
        "값은 1이다.\n참이면:\n    값을 2로 바꾼다.\n\n값\n없는값\n(\n:취소\n:초기화\n3\n",
    );
    assert_eq!((code, stdout.as_str()), (Some(1), "2\n3\n"));
    assert!(stderr.contains("E-RESOLVE-001"), "{stderr}");
}

#[test]
fn errors_are_reported_where_they_happen() {
    let source = "나누기계산하는 법:\n    (1 // 0)을 반환한다.\n시작하는 법:\n    나누기계산한다.\n시작한다.\n";
    let error = ieum::execute(source.as_bytes(), &mut Vec::new(), &Options::default()).unwrap_err();
    let mut report = Vec::new();
    error
        .write_to("예.이음", source.as_bytes(), &mut report)
        .unwrap();
    assert_eq!(
        String::from_utf8(report).unwrap(),
        "예.이음:2:8: E-RUNTIME-002: 0으로 나눌 수 없습니다.\n  2 |     (1 // 0)을 반환한다.\n    |        ^^\n  호출 경로 (가장 안쪽부터):\n    나누기계산하다: 예.이음:4:5\n    시작하다: 예.이음:5:1\n"
    );
    let source = "1을 출력한다.\n첫째는 (1 + )이다.\n둘째는 (2 * )이다.\n";
    let lines: Vec<_> = ieum::check(source.as_bytes())
        .iter()
        .map(|error| error.location(source.as_bytes()).0)
        .collect();
    assert_eq!(lines, [2, 3]);
    let deep = format!(
        "{}1{}을 출력한다.\n",
        "(".repeat(100_000),
        ")".repeat(100_000)
    );
    assert_eq!(ieum::check(deep.as_bytes())[0].code, "E-LIMIT-001");
}

#[test]
fn programs_reach_outside_only_when_allowed() {
    let dir = TempDir::new("reach");
    let files = FileSystem::new(&dir.0).unwrap();
    let closed = Options::default();
    let open = Options {
        filesystem: Some(&files),
        ..Default::default()
    };
    let write = "\"내용\"을 \"글.txt\"에 쓴다.\n(\"글.txt\"를 읽은 값)을 출력한다.\n";
    assert_eq!(execute(write, &closed), "E-FILE-003");
    assert_eq!(execute(write, &open), "내용\n");
    assert_eq!(execute("[\"ieum\"]을 실행한다.\n", &closed), "E-PROC-003");
    assert_eq!(
        execute("'http://127.0.0.1:9'에서 내려받는다.\n", &closed),
        "E-NET-003"
    );
    let source = "늘리는 법:\n    목록에 []를 추가한다.\n    늘린다.\n목록은 []이다.\n늘린다.\n";
    for limits in [
        Limits {
            max_steps: 100,
            ..Limits::default()
        },
        Limits {
            max_call_depth: 8,
            ..Limits::default()
        },
        Limits {
            max_heap_objects: 10,
            ..Limits::default()
        },
    ] {
        let options = Options {
            limits,
            ..Default::default()
        };
        assert_eq!(execute(source, &options), "E-LIMIT-001");
    }
}

#[test]
fn formatting_keeps_every_written_byte_that_matters() {
    let source = "\u{feff}# 머리\r\n상수   `가`는 ( 1+2* - 3 )이다 .   # 꼬리\r\n`가`를   출력한다 .\r\n( 5와  3을 합산한  값 )을 출력하고\r\n# 다음 절\r\n출력한다 .";
    let formatted = format_source(source.as_bytes()).unwrap();
    assert_eq!(
        formatted,
        "\u{feff}# 머리\n상수 `가`는 (1 + 2 * -3)이다.  # 꼬리\n`가`를 출력한다.\n(5와 3을 합산한 값)을 출력하고\n# 다음 절\n출력한다.\n"
    );
    assert_eq!(format_source(formatted.as_bytes()).unwrap(), formatted);
    for text in [source, "", "(", "[1)", "값\0", "참이면:\n  1을 출력한다.\n"] {
        assert_eq!(parse_syntax(text.as_bytes()).unwrap().reconstruct(), text);
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ieum"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next: 0,
        };
        client.request("initialize", json!({"capabilities": {}}));
        client.notify("initialized", json!({}));
        client
    }

    fn notify(&mut self, method: &str, params: Json) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn send(&mut self, message: Json) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Json {
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(self.stdout.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, method: &str, params: Json) -> Json {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.receive();
            if message["id"] == id {
                return message["result"].clone();
            }
        }
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "ieum", "version": 1, "text": text}}),
        );
    }
}

fn file_uri(path: &Path) -> String {
    let path = ieum::display_path(&fs::canonicalize(path).unwrap()).replace('\\', "/");
    let encoded: String = path
        .trim_start_matches('/')
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'/' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!("file:///{encoded}")
}

fn marked_words(text: &str, tokens: &Json) -> Vec<(String, u64)> {
    let lines: Vec<Vec<u16>> = text.lines().map(|l| l.encode_utf16().collect()).collect();
    let (mut line, mut column) = (0, 0);
    let mut marked = Vec::new();
    for token in tokens["data"].as_array().unwrap().chunks(5) {
        let number = |i: usize| token[i].as_u64().unwrap() as usize;
        if number(0) > 0 {
            column = 0;
        }
        line += number(0);
        column += number(1);
        let word = String::from_utf16(&lines[line][column..column + number(2)]).unwrap();
        marked.push((word, number(3) as u64));
    }
    marked
}

#[test]
fn language_server() {
    let dir = TempDir::new("lsp");
    let tool_text = "`수`를 두배하는 법:\n    (`수` * 2)를 반환한다.\n";
    let tool = file_uri(&dir.put("도구.이음", tool_text));
    let text = "\"도구\"에서 두배하다를 가져온다.
합계는 0이다.
`수`를 늘리는 법:
    (`수` + 1)을 반환한다.
합계를 (합계를 늘린 값)으로 바꾼다.
[1, 2] 중 원소마다:
    원소를 출력한다.
(3을 두배한 값)을 출력한다.
합게를 출력한다.
";
    let main = file_uri(&dir.put("시작.이음", text));
    let mut client = Client::start();
    client.open(&main, text);
    let diagnostics = client.receive()["params"]["diagnostics"].clone();
    assert_eq!(diagnostics[0]["code"], "E-RESOLVE-001");
    let at = |line: u32, character: u32| json!({"textDocument": {"uri": main}, "position": {"line": line, "character": character}});

    let hover = client.request("textDocument/hover", at(4, 10));
    let shown = hover["contents"]["value"].as_str().unwrap();
    assert!(shown.contains("`늘린`은 `늘리다`의 활용형"), "{shown}");
    let definition = client.request("textDocument/definition", at(7, 5));
    assert_eq!(definition["uri"], tool);
    let mut place = at(1, 0);
    place["context"] = json!({"includeDeclaration": true});
    let references = client.request("textDocument/references", place);
    assert_eq!(references.as_array().unwrap().len(), 3);
    let mut place = at(2, 6);
    place["newName"] = json!("키우다");
    let renamed = client.request("textDocument/rename", place);
    let edits: Vec<_> = renamed["changes"][&main]
        .as_array()
        .unwrap()
        .iter()
        .map(|edit| edit["newText"].as_str().unwrap())
        .collect();
    assert_eq!(edits, ["키우는", "키운"]);

    let mut place = at(6, 8);
    place["context"] = json!({"triggerKind": 2, "triggerCharacter": " "});
    let mut items = client.request("textDocument/completion", place);
    let items = items.as_array_mut().unwrap();
    items.sort_by_key(|item| item["sortText"].as_str().unwrap_or_default().to_owned());
    assert_eq!(items[0]["label"], "출력한다");

    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument": {"uri": main}}),
    );
    let marked = marked_words(text, &tokens);
    for (word, kind) in [
        ("가져온다", 5),
        ("늘리는", 0),
        ("합계", 1),
        ("수", 2),
        ("반환한다", 6),
        ("중", 6),
        ("마다", 6),
    ] {
        assert!(
            marked.contains(&(word.to_owned(), kind)),
            "{word} {marked:?}"
        );
    }

    let range = json!({"start": {"line": 8, "character": 0}, "end": {"line": 8, "character": 2}});
    let fix =
        json!({"range": range, "message": "", "data": {"fix": {"range": range, "text": "합계"}}});
    let actions = client.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": main}, "range": range, "context": {"diagnostics": [fix]}}),
    );
    assert_eq!(actions[0]["title"], "'합계'로 고치기");

    client.open(&tool, &format!("\n{tool_text}"));
    let definition = client.request("textDocument/definition", at(7, 5));
    assert_eq!(definition["range"]["start"]["line"], 1);

    client.request("shutdown", Json::Null);
    client.notify("exit", Json::Null);
    assert!(client.child.wait().unwrap().success());
}
